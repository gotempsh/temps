# Temps CLI Reference

> Auto-generated documentation for the Temps CLI.
>
> Generated from: `@temps-sdk/cli@0.2.0-beta.1`
>
> Apply the authorization, target-context, and secret-handling rules in
> [the Temps CLI skill](../SKILL.md) before executing a command.

## Installation

```bash
bunx @temps-sdk/cli@beta [command]

# Fallback when Bun is unavailable
npx @temps-sdk/cli@beta [command]
```

## Authentication

Before using most commands, you need to authenticate:

```bash
# Login interactively
bunx @temps-sdk/cli@beta login

# Or configure with wizard
bunx @temps-sdk/cli@beta configure
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

- [`login`](#login) - Authenticate with a Temps server. Opens the browser for interactive logins; use --no-wait then --resume when a person approves but the caller cannot keep a command open, and --api-key for headless / CI.
- [`logout`](#logout) - Revoke the active context's API key on the server and forget it locally
- [`whoami`](#whoami) - Display current authenticated user and active context
- [`context`](#context) - Manage CLI contexts (one set of credentials per Temps server)
- [`configure`](#configure) - Configure CLI settings (AWS-style wizard)
- [`projects`](#projects-alias-project-p) - Manage services: each one deployable app (the console calls these Services). To group services that ship together into a Project, as the console calls it, use `project-groups`.
- [`drop`](#drop) - Detect and deploy a local source directory or ZIP without Git
- [`deploy`](#deploy) - Deploy a project from git
- [`deploy:static`](#deploystatic-alias-deploy-static) - Deploy static files (tar.gz, zip, or directory)
- [`deploy:image`](#deployimage-alias-deploy-image) - Deploy a pre-built Docker image
- [`deploy:local-image`](#deploylocal-image-alias-deploy-local-image) - Build and deploy a local Docker image (or deploy existing image with --image)
- [`deployments`](#deployments-alias-deploys) - Manage deployments
- [`domains`](#domains-alias-domain) - Manage custom domains
- [`environments`](#environments-alias-envs-env) - Manage environments and environment variables
- [`providers`](#providers-alias-provider) - Manage Git providers
- [`backups`](#backups-alias-backup) - Manage backup schedules and backups
- [`runtime-logs`](#runtime-logs-alias-rlogs) - View runtime container logs (use -f to follow in real-time)
- [`notifications`](#notifications-alias-notify) - Manage notification providers (Slack, Email, Webhook, Telegram, etc.)
- [`dns`](#dns) - Manage DNS providers and Temps-managed DNS records
- [`services`](#services-alias-svc) - Manage external services (databases, caches, storage)
- [`settings`](#settings) - Manage platform settings
- [`users`](#users) - Manage platform users
- [`teams`](#teams) - Manage teams and project access
- [`access`](#access) - Manage which teams can reach a project
- [`project-groups`](#project-groups-alias-pg) - Manage Projects (what the console calls a Project): groups of services that ship together and share a repository, variables and path routes. A service is what the `projects` command manages; the `services` command manages databases and caches, not these.
- [`instantiations`](#instantiations-alias-inst) - Follow and retry template creations and imports
- [`apikeys`](#apikeys-alias-keys) - Manage API keys for programmatic access
- [`monitors`](#monitors-alias-monitoring) - Manage uptime monitors for status pages
- [`webhooks`](#webhooks-alias-hooks) - Manage webhooks for project events
- [`containers`](#containers-alias-cts) - Manage project containers in environments
- [`cluster`](#cluster) - Cluster-wide multi-node operations
- [`nodes`](#nodes) - Worker nodes and workload placement
- [`tunnels`](#tunnels) - Ingress tunnels: publish apps on workers with no open ports or certificates. Apps under a tunnel's domain go live by adding the hostname to an environment
- [`tokens`](#tokens-alias-token) - Manage deployment tokens for project API access (KV, Blob, etc.)
- [`errors`](#errors-alias-error) - Manage error tracking and error groups
- [`metrics`](#metrics-alias-metric) - Query OTel application metrics for debugging (not container/docker stats — see "temps containers metrics" for those)
- [`server`](#server) - Resource usage of the machine running the Temps control plane (what /monitoring/server shows)
- [`traces`](#traces-alias-trace) - Inspect distributed traces and operation latency
- [`facets`](#facets) - Manage OTel span attribute facets — attribute keys promoted to a fast-filterable column (ClickHouse or TimescaleDB, whichever backend is active; see ADR-039). Facets are platform-global, not per-project, since the underlying spans table is shared across every project. Historical backfill runs asynchronously — check `temps facets list` for status.
- [`otel-forward`](#otel-forward) - Manage OTel forwarding destinations that relay ingested traces, metrics, and logs to an external OTLP-compatible collector
- [`otel`](#otel) - Inspect the OTLP ingest pipeline itself — throughput, drops and failure reasons (server-wide, not project-scoped; see "temps metrics" to query ingested application metrics)
- [`kv`](#kv) - Read and write a project's keys in the KV store, and manage the KV service
- [`flags`](#flags-alias-flag) - Manage feature flags (runtime config that changes without a redeploy)
- [`surveys`](#surveys-alias-survey) - Manage in-app surveys (NPS, ratings, choices, open text) and read their answers
- [`data`](#data) - Browse the data inside a service (tables, collections, keys, objects) — read-only
- [`blob`](#blob) - Upload, download and list a project's files in Blob storage, and manage the Blob service
- [`dsn`](#dsn) - Manage Data Source Names (DSNs) for error tracking and analytics
- [`scans`](#scans-alias-scan) - Manage vulnerability scans
- [`custom-domains`](#custom-domains-alias-cdom) - Manage project custom domains
- [`dns-provider`](#dns-provider-alias-dnsp) - Manage DNS providers and managed domains
- [`delivery-profiles`](#delivery-profiles-alias-delivery-profile) - Manage traffic delivery profiles (Cloudflare proxy, Bunny CDN) used by project domains
- [`delivery`](#delivery) - Route project domains through a delivery provider (Cloudflare proxy, Bunny CDN)
- [`ip-access`](#ip-access-alias-ipa) - Manage instance-wide IP access control rules, and per-project lists (ip-access project)
- [`preview-gateway`](#preview-gateway-alias-pgw) - Manage the preview gateway that serves workspace preview URLs (status, settings, logs, restart, upgrade)
- [`edge-rules`](#edge-rules-alias-edge-rule) - Block, allow, challenge, redirect or log requests before they reach the app. Rules run top to bottom; the first match decides
- [`network-policy`](#network-policy-alias-netpol) - Manage network policy for workloads: rules, egress/ingress profiles and the effective policy
- [`audit`](#audit) - View audit logs
- [`proxy-logs`](#proxy-logs-alias-plogs) - View proxy request logs and statistics
- [`logs`](#logs-alias-glogs) - Search collected logs across every project and database you can access
- [`email-domains`](#email-domains-alias-edom) - Manage email domains for transactional email
- [`email-providers`](#email-providers-alias-eprov) - Manage email providers (SES, Scaleway) for transactional email
- [`incidents`](#incidents-alias-incident) - Manage incidents for status pages and monitoring
- [`status-pages`](#status-pages-alias-status-page) - Publish public status pages showing monitors from any of your projects
- [`alarms`](#alarms-alias-alarm) - List, acknowledge, and resolve alarms (container crashes, uptime, metrics, databases)
- [`emails`](#emails-alias-email) - Manage and send emails
- [`load-balancer`](#load-balancer-alias-lb) - Manage load balancer routes
- [`migrate`](#migrate-alias-imports-import) - Migrate a project from another platform (Vercel, Coolify, Dokploy, CapRover, Portainer, Kubernetes, Docker) into temps
- [`templates`](#templates-alias-tpl) - Browse templates and create them
- [`platform`](#platform-alias-plat) - View platform and server information
- [`presets`](#presets-alias-preset) - Browse available build presets
- [`analytics`](#analytics-alias-stats) - View project analytics
- [`funnels`](#funnels-alias-funnel) - Manage analytics funnels for projects
- [`notification-preferences`](#notification-preferences-alias-notif-prefs) - Manage notification preferences
- [`skills`](#skills-alias-skill) - Manage AI skill definitions (global or project-scoped)
- [`mcp-servers`](#mcp-servers) - Manage MCP server definitions (global or project-scoped)
- [`mcp`](#mcp) - Configure this Temps instance as an MCP server for AI clients (Claude Code, Claude Desktop, Codex, Cursor, VS Code, Windsurf, Zed)
- [`secrets`](#secrets-alias-secret) - Manage agent secrets. env-type: reference as ${TEMPS_SECRET:name} in MCP config. file-type: written to --mount-path in sandbox; reference that path.
- [`sandbox`](#sandbox) - Manage standalone sandboxes (/v1/sandbox API)
- [`workflow`](#workflow-alias-wf) - Trigger and inspect agent/workflow runs
- [`workflows`](#workflows) - Durable application workflows
- [`revenue`](#revenue) - Manage revenue integrations and import historical data
- [`session-replay`](#session-replay-alias-sessions-replay) - Manage session replay recordings
- [`traefik-discovery`](#traefik-discovery) - Route containers Temps did not deploy by reading their Traefik labels (an existing docker-compose / Coolify / Dokploy stack)
- [`init`](#init) - Initialize a Temps project in the current directory
- [`link`](#link) - Link current directory to a Temps project
- [`up`](#up) - Deploy the current project (runs setup wizard if not linked)
- [`status`](#status) - Show project deployment status
- [`ai`](#ai) - AI assistant status for a project
- [`instances`](#instances-alias-instance) - Manage Temps server instances
- [`env:pull`](#envpull) - Pull environment variables to a .env file
- [`env:push`](#envpush) - Push environment variables from a .env file
- [`rollback`](#rollback) - Rollback to a previous deployment
- [`open`](#open) - Open project URL in browser
- [`cloud`](#cloud) - Temps Cloud
- [`plugin`](#plugin) - Create and build TypeScript plugins, or install and manage external plugins
- [`setup`](#setup) - PoC: install Temps on an existing Linux VPS over SSH and save a client context
- [`start`](#start) - Run Temps on this computer, sign in the CLI and add the Temps skill for your coding agent
- [`stop`](#stop) - Stop the Temps server and database started by `temps start`
- [`agents`](#agents) - Project agents, their MCP servers, skills and AI providers
- [`shield`](#shield) - Shield: bot and abuse protection rules, entries and proposals
- [`vault`](#vault) - Access vaults, the services they hold and their sessions

## Commands

## `login`

Authenticate with a Temps server. Opens the browser for interactive logins; use --no-wait then --resume when a person approves but the caller cannot keep a command open, and --api-key for headless / CI.

**Usage:** `login [url] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-k, --api-key <key>` | Use a pre-minted API key (Settings → API Keys) instead of opening the browser. Required for headless / CI. | - | No |
| `--context <name>` | Save the credentials under this context name (defaults to URL host) | - | No |
| `--no-wait` | Print the browser approval link and exit instead of waiting for it. For agents and scripts whose shell returns output only when a command exits; finish with --resume. | - | No |
| `--resume` | Finish a sign-in started with --no-wait: wait for the browser approval, then save the credentials. Exits with code 2 if it is not approved yet; run it again. | - | No |
| `--wait-secs <seconds>` | With --resume: how long to wait for the approval before exiting with code 2 (default: 90) | - | No |
| `--debug` | Print every request/response (URL, status, headers, raw body) to stderr. Also enabled via TEMPS_DEBUG=1. | - | No |
| `--api-key-file <path>` | Read the API key from a file (- reads stdin) instead of --api-key | - | No |
| `--api-key-stdin` | Read the API key from stdin instead of --api-key | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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

**Usage:** `context use <name>`

### `context remove` (alias: `rm`)

Remove a context (does NOT revoke the key on the server)

**Usage:** `context remove <name>`

### `context rename`

Rename a context

**Usage:** `context rename <old-name> <new-name>`

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
| `--api-token-file <path>` | Read the API token from a file (- reads stdin) instead of --api-token | - | No |
| `--api-token-stdin` | Read the API token from stdin instead of --api-token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

**Subcommands:**

- `get` - Get a configuration value
- `set` - Set a configuration value
- `list` - List all configuration values
- `show` - Show current configuration and authentication status
- `reset` - Reset configuration to defaults

### `configure get`

Get a configuration value

**Usage:** `configure get <key>`

### `configure set`

Set a configuration value

**Usage:** `configure set <key> <value>`

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

Manage services: each one deployable app (the console calls these Services). To group services that ship together into a Project, as the console calls it, use `project-groups`.

**Subcommands:**

- `secrets` - Manage project secrets — mounted into the deployed container as files at /run/secrets/<KEY>, not environment variables. Distinct from `temps secrets` (agent/MCP-sandbox-scoped).
- `list` (`ls`) - List all services
- `create` (`new`) - Create a new service (git-based or manual deployment); add it to a project with `project-groups add`
- `cloudflare-capability` (`delivery-capability`) - Show whether Cloudflare/Bunny delivery is ready and which one new services get by default
- `show` (`get`) - Show service details, including the project (group) it belongs to
- `group` - Show which project (group of services) a service belongs to
- `update` (`edit`) - Update service name and description
- `settings` - Update service settings (name, slug, attack mode, preview environments, vulnerability scanning, image retention)
- `git` - Update git repository settings
- `source` - Show or change how a service is deployed (primary source, and whether it also accepts `drop` uploads)
- `hosting` - Show or set whether Temps runs the app (hosted) or it runs elsewhere (external, labelled External and never deployed)
- `container-security` - Show or change which Linux capabilities the project's deployment containers get back (only NET_BIND_SERVICE can be allowed; Compose services and microVMs are not affected)
- `identical-builds` - Show or change what the project's builds do when an identical build (same repository, commit, build path and build-time values) is already running: build in parallel, queue, or build once and share the image
- `config` - Update deployment configuration (resources, replicas)
- `delete` (`rm`) - Delete a service
- `create-from-template` - Create a new project from a template
- `get-statistics` - Get project statistics
- `get-compose-security` - Get compose security
- `update-compose-security` - Update compose security
- `update-automatic-deploy` - Update automatic deployment setting for a project
- `get-resolved-environment-variables` - Resolved env vars for a project (manual + integration-sourced, merged)
- `get-resolved-environment-variable-value` - Reveal the plaintext value of a resolved environment variable
- `get-environment-variable` - Get one environment variable of a project by its ID
- `dismiss-environment-variable-secret-suggestion` - Dismiss the "store as secret" suggestion for a regular variable
- `get-environment` - Get a specific environment by ID or slug
- `get-environment-domains` - Get all environment domains for a specific environment
- `add-environment-domain` - Add a new environment domain
- `delete-environment-domain` - Delete an environment domain
- `update-environment-subdomain` - Rename the auto-managed subdomain for an environment
- `teardown-environment` - Teardown an environment and all its active deployments
- `reinstall-github-webhook` - Install or reinstall the GitHub repository webhook for a project
- `reinstall-gitlab-webhook` - Reinstall the GitLab webhook for a project
- `get-ingest-setup` - Get where this project's apps send telemetry
- `update-service-template-runtime` - Update service template runtime
- `get-service-template` - Return the immutable service-template release applied to a project together with catalog drift, missing..
- `upgrade-service-template` - Upgrade project service template

### `projects secrets`

Manage project secrets — mounted into the deployed container as files at /run/secrets/<KEY>, not environment variables. Distinct from `temps secrets` (agent/MCP-sandbox-scoped).

**Subcommands:**

- `get` - Show one project secret's metadata (the value is never returned)
- `list` (`ls`) - List secrets for a project (values are never returned)
- `create` (`add`) - Create a project secret (mounted at /run/secrets/<KEY> on the next deployment). Read the value with --value-file or --value-stdin to keep it out of shell history
- `update` - Update a project secret (a redeploy is required for running containers to pick it up)
- `delete` (`rm`) - Delete a project secret

#### `projects secrets get`

Show one project secret's metadata (the value is never returned)

**Usage:** `projects secrets get <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `projects secrets list` (alias: `ls`)

List secrets for a project (values are never returned)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Filter to one environment | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `projects secrets create` (alias: `add`)

Create a project secret (mounted at /run/secrets/<KEY> on the next deployment). Read the value with --value-file or --value-stdin to keep it out of shell history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-k, --key <key>` | Secret key — becomes the filename at /run/secrets/<KEY>. Letters, digits, underscore; must start with a letter or underscore. | - | Yes |
| `-v, --value <value>` | Secret value on the command line (<=1 MiB; visible in shell history and `ps`). `-v @path` reading a file is deprecated: use --value-file | - | No |
| `-e, --environment <name>` | Scope to one environment (repeatable; default: all) | `` | No |
| `-s, --service <name>` | Docker Compose service allowed to read this secret (repeatable; default: every service). Ignored for non-Compose projects, which deploy a single container. | `` | No |
| `--include-in-preview` | Also mount this secret in preview environments | - | No |
| `--value-file <path>` | Read the secret value (<=1 MiB) from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the secret value (<=1 MiB) from stdin, e.g. piped from a secret manager | - | No |
| `--literal` | Send a -v value that starts with @ as is, instead of reading it as a file path (deprecated form) | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `projects secrets update`

Update a project secret (a redeploy is required for running containers to pick it up)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-k, --key <key>` | Key of the secret to update | - | Yes |
| `-v, --value <value>` | New value on the command line (<=1 MiB; visible in shell history). `-v @path` is deprecated: use --value-file. Give no value source to keep the existing value. | - | No |
| `-e, --environment <name>` | Replace environment scoping (repeatable) | `` | No |
| `-s, --service <name>` | Replace the Docker Compose service scope (repeatable). Pass none to keep the current scope; use --all-services to widen it back to every service. | `` | No |
| `--all-services` | Deliver to every Compose service, clearing any per-service scope | - | No |
| `--include-in-preview` | Include in preview environments | - | No |
| `--no-include-in-preview` | Exclude from preview environments | - | No |
| `--value-file <path>` | Read the new secret value (<=1 MiB) from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the new secret value (<=1 MiB) from stdin, e.g. piped from a secret manager | - | No |
| `--literal` | Send a -v value that starts with @ as is, instead of reading it as a file path (deprecated form) | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `projects secrets delete` (alias: `rm`)

Delete a project secret

**Usage:** `projects secrets delete <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `projects list` (alias: `ls`)

List all services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page | - | No |
| `--group <id>` | Only services in this project group (its numeric ID) | - | No |
| `--ungrouped` | Only services in no project group | - | No |

### `projects create` (alias: `new`)

Create a new service (git-based or manual deployment); add it to a project with `project-groups add`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Service name | - | No |
| `-d, --description <description>` | Service description | - | No |
| `--repo <repository>` | Repository in owner/name format (nested groups supported: group/subgroup/name) | - | No |
| `--branch <branch>` | Git branch | - | No |
| `--directory <directory>` | Root directory (relative to repo) | - | No |
| `--preset <preset>` | Build preset (e.g., nextjs, nodejs, static, docker) | - | No |
| `--connection <id>` | Git connection ID | - | No |
| `--manual` | Create a manual (non-git) service - deploy via Docker image or static files | - | No |
| `--source-type <type>` | Manual deployment method: manual (flexible), docker_image, or static_files | - | No |
| `--image <image>` | Docker image for the first deployment (manual mode) | - | No |
| `--port <port>` | Application/container port (manual mode, default: 3000) | - | No |
| `--external` | The app runs elsewhere (Vercel, Netlify, a VPS): Temps only receives its telemetry and never deploys it (needs --name with --yes) | - | No |
| `--delivery-provider <provider>` | Delivery provider for the new service: none, cloudflare or bunny (default: instance setting; see `projects cloudflare-capability`) | - | No |
| `-y, --yes` | Skip optional prompts (services, env vars, set-default) | - | No |
| `--cloudflare-enabled` | Serve through Cloudflare (overrides the instance default) | - | No |
| `--no-cloudflare-enabled` | Set cloudflare_enabled to false | - | No |
| `--custom-domain <value>` | Custom domain to attach to the production environment | - | No |
| `--expected-slug <value>` | Fail unless the project gets exactly this URL slug | - | No |
| `--is-on-demand` | Start containers on the first request and stop them when idle | - | No |
| `--no-is-on-demand` | Set is_on_demand to false | - | No |
| `--is-public-repo` | The repository is public (cloned without a git connection) | - | No |
| `--no-is-public-repo` | Set is_public_repo to false | - | No |
| `--is-web-app` | The app serves HTTP (false for a worker with no port) | - | No |
| `--no-is-web-app` | Set is_web_app to false | - | No |
| `--performance-metrics-enabled` | Collect web performance metrics | - | No |
| `--no-performance-metrics-enabled` | Set performance_metrics_enabled to false | - | No |
| `--use-default-wildcard` | Give environments a hostname under the instance's wildcard domain | - | No |
| `--no-use-default-wildcard` | Set use_default_wildcard to false | - | No |

### `projects cloudflare-capability` (alias: `delivery-capability`)

Show whether Cloudflare/Bunny delivery is ready and which one new services get by default

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `projects show` (alias: `get`)

Show service details, including the project (group) it belongs to

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Service slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `projects group`

Show which project (group of services) a service belongs to

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Service slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `projects update` (alias: `edit`)

Update service name and description

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | New project name | - | No |
| `-d, --description <description>` | New project description | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts, use provided values (for automation) | - | No |
| `--build-command <value>` | Build command (overrides the preset) | - | No |
| `--install-command <value>` | Install command (overrides the preset) | - | No |
| `--output-dir <value>` | Build output directory (static sites) | - | No |
| `--cloudflare-enabled` | Serve through Cloudflare | - | No |
| `--no-cloudflare-enabled` | Set cloudflare_enabled to false | - | No |
| `--custom-domain <value>` | Custom domain of the production environment | - | No |
| `--expected-slug <value>` | Fail unless the project keeps exactly this URL slug | - | No |
| `--is-on-demand` | Start containers on the first request and stop them when idle | - | No |
| `--no-is-on-demand` | Set is_on_demand to false | - | No |
| `--is-public-repo` | The repository is public (cloned without a git connection) | - | No |
| `--no-is-public-repo` | Set is_public_repo to false | - | No |
| `--is-web-app` | The app serves HTTP (false for a worker with no port) | - | No |
| `--no-is-web-app` | Set is_web_app to false | - | No |
| `--performance-metrics-enabled` | Collect web performance metrics | - | No |
| `--no-performance-metrics-enabled` | Set performance_metrics_enabled to false | - | No |
| `--preset-config <json>` | Preset-specific build settings as JSON, e.g. {"buildCommand":"bun run build"} (JSON) | - | No |
| `--use-default-wildcard` | Give environments a hostname under the instance's wildcard domain | - | No |
| `--no-use-default-wildcard` | Set use_default_wildcard to false | - | No |

### `projects settings`

Update service settings (name, slug, attack mode, preview environments, vulnerability scanning, image retention)

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
| `--vulnerability-scanning` | Enable Trivy vulnerability scanning of deployed Docker images (post-deploy + daily) | - | No |
| `--no-vulnerability-scanning` | Disable vulnerability scanning | - | No |
| `--image-retention-hours <hours>` | Hours to keep built images before nightly cleanup removes them (1-8760). Images are needed to roll back, so this is the project rollback window | - | No |
| `--reset-image-retention` | Clear the per-project image retention override and use the system default | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts (for automation) | - | No |
| `--ai-alert-summaries-enabled` | Summarize metric alert notifications with AI | - | No |
| `--no-ai-alert-summaries-enabled` | Set ai_alert_summaries_enabled to false | - | No |
| `--ai-api-traffic-summary-enabled` | Summarize API traffic analytics with AI | - | No |
| `--no-ai-api-traffic-summary-enabled` | Set ai_api_traffic_summary_enabled to false | - | No |
| `--cross-project-trace-sharing` | Show this project's spans in other projects' traces | - | No |
| `--no-cross-project-trace-sharing` | Set cross_project_trace_sharing to false | - | No |
| `--error-source-context-enabled` | Show source code in error stack traces (uploads source files) | - | No |
| `--no-error-source-context-enabled` | Set error_source_context_enabled to false | - | No |
| `--error-source-root <value>` | Source root for error source context, relative to the checkout | - | No |
| `--log-ingest-lines-per-sec <n>` | Log ingestion budget for the project's containers, lines per second (0 lifts it) | - | No |
| `--log-ingest-kib-per-sec <n>` | Log ingestion budget in KiB per second (0 lifts it) | - | No |
| `--preview-envs-on-demand` | New preview environments start on demand and stop when idle | - | No |
| `--no-preview-envs-on-demand` | Set preview_envs_on_demand to false | - | No |
| `--preview-envs-idle-timeout-seconds <n>` | Idle seconds (60-86400) before an on-demand preview stops | - | No |
| `--preview-envs-wake-timeout-seconds <n>` | Seconds (5-120) to wait for an on-demand preview to wake | - | No |
| `--serves-own-temps-api` | Send /api/_temps* and /api/otel* on the project's domains to the app instead of Temps | - | No |
| `--no-serves-own-temps-api` | Set serves_own_temps_api to false | - | No |
| `--shield-mode <value>` | How Shield acts on its verdicts (one of: off, log, selective, attack) | - | No |

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
| `--is-public-repo` | The repository is public: clone it without a git connection | - | No |
| `--no-is-public-repo` | Set is_public_repo to false | - | No |
| `--git-url <value>` | Clone URL of a public repository | - | No |
| `--preset-config <json>` | Preset-specific build settings as JSON, e.g. {"buildCommand":"bun run build"} (JSON) | - | No |
| `--pull-only-root-directory` | Clone only --directory instead of the whole repository | - | No |
| `--no-pull-only-root-directory` | Set pull_only_root_directory to false | - | No |

### `projects source`

Show or change how a service is deployed (primary source, and whether it also accepts `drop` uploads)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--type <type>` | Set the primary source: docker_image, static_files, uploaded_source or manual (use `projects git` to switch to git) | - | No |
| `--allow-alternate` | Also accept an uploaded source archive from `drop`, keeping the current source as default | - | No |
| `--no-allow-alternate` | Only deploy from the configured source | - | No |
| `--json` | Output in JSON format | - | No |

### `projects hosting`

Show or set whether Temps runs the app (hosted) or it runs elsewhere (external, labelled External and never deployed)

**Usage:** `projects hosting [hosting] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `projects container-security`

Show or change which Linux capabilities the project's deployment containers get back (only NET_BIND_SERVICE can be allowed; Compose services and microVMs are not affected)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--allow <capabilities...>` | Allow these capabilities, e.g. NET_BIND_SERVICE (asks for confirmation) | - | No |
| `--remove <capabilities...>` | Stop allowing these capabilities | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip the confirmation (for automation) | - | No |

### `projects identical-builds`

Show or change what the project's builds do when an identical build (same repository, commit, build path and build-time values) is already running: build in parallel, queue, or build once and share the image

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--mode <mode>` | parallel (default), queue or share | - | No |
| `--scope <scope>` | Which builds wait for each other: source (same repository and path, the default) or fingerprint (only identical builds) | - | No |
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
| `--build-location <value>` | Where a source deployment's image is built (one of: control_plane, node, ci) | - | No |
| `--cross-architecture-builds` | Build one image per architecture the eligible nodes run | - | No |
| `--no-cross-architecture-builds` | Set crossArchitectureBuilds to false | - | No |
| `--cpu-request <n>` | CPU reserved per container, in microcores | - | No |
| `--memory-request <n>` | Memory reserved per container, in MB | - | No |
| `--exposed-port <n>` | Port the container listens on | - | No |
| `--health <json>` | Health check settings as JSON, e.g. {"path":"/healthz"} (JSON) | - | No |
| `--health-check-timeout-seconds <n>` | Seconds (30-3600) a new deployment's containers get to become ready | - | No |
| `--stop-timeout-seconds <n>` | Seconds (1-3600) the previous deployment gets to finish in-flight requests | - | No |
| `--load-balancing <json>` | Load balancing settings as JSON (JSON) | - | No |
| `--max-concurrent-connections <n>` | Default cap on concurrent requests to an environment's upstream (0 = unlimited) | - | No |
| `--performance-metrics-enabled` | Collect web performance metrics | - | No |
| `--no-performance-metrics-enabled` | Set performanceMetricsEnabled to false | - | No |
| `--session-recording-enabled` | Record visitor sessions | - | No |
| `--no-session-recording-enabled` | Set sessionRecordingEnabled to false | - | No |

### `projects delete` (alias: `rm`)

Delete a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `projects create-from-template`

Create a new project from a template

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--automatic-deploy` | Enable automatic deployment on push (defaults to true) | - | No |
| `--no-automatic-deploy` | Set automatic_deploy to false | - | No |
| `--cloudflare-enabled` | Override the Cloudflare default for this new project | - | No |
| `--no-cloudflare-enabled` | Set cloudflare_enabled to false | - | No |
| `--command <value>` | Optional image entrypoint arguments (repeatable) | - | No |
| `--cpu-limit <n>` | CPU limit override in microcores | - | No |
| `--cpu-request <n>` | CPU request override in microcores (1_000_000 = one CPU core) | - | No |
| `--delivery-provider <value>` | Body field delivery_provider | - | No |
| `--environment-variables <json>` | Environment variables to set (key-value pairs) (JSON) | - | No |
| `--exposed-port <n>` | Public container port override | - | No |
| `--git-provider-connection-id <n>` | Git provider connection ID | - | No |
| `--health-check-path <value>` | Relative HTTP health-check path override | - | No |
| `--image <value>` | Optional prebuilt-image override | - | No |
| `--memory-limit <n>` | Memory limit override in MiB | - | No |
| `--memory-request <n>` | Memory request override in MiB | - | No |
| `--private` | Whether to make the repository private (defaults to true) | - | No |
| `--no-private` | Set private to false | - | No |
| `--project-name <value>` | Name for the new project [required] | - | No |
| `--repository-name <value>` | Name for the new repository to create | - | No |
| `--repository-owner <value>` | Owner/organization for the new repository (defaults to authenticated user) | - | No |
| `--storage-service-ids <n>` | External storage service IDs to attach to the project (repeatable) | - | No |
| `--template-slug <value>` | Template slug to use as the base [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects get-statistics`

Get project statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `projects get-compose-security`

Get compose security

**Usage:** `projects get-compose-security <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `projects update-compose-security`

Update compose security

**Usage:** `projects update-compose-security <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--acknowledge-legacy-migration` | Body field acknowledge_legacy_migration | - | No |
| `--no-acknowledge-legacy-migration` | Set acknowledge_legacy_migration to false | - | No |
| `--acknowledge-risks` | Body field acknowledge_risks | - | No |
| `--no-acknowledge-risks` | Set acknowledge_risks to false | - | No |
| `--expected-policy <json>` | Policy read by the editor; compared while holding the project lock (JSON) [required] | - | No |
| `--policy <json>` | Body field policy (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects update-automatic-deploy`

Update automatic deployment setting for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--automatic-deploy` | Body field automatic_deploy [required] | - | No |
| `--no-automatic-deploy` | Set automatic_deploy to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects get-resolved-environment-variables`

Resolved env vars for a project (manual + integration-sourced, merged)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Optional environment ID to filter manual vars by | - | No |
| `--json` | Output in JSON format | - | No |

### `projects get-resolved-environment-variable-value`

Reveal the plaintext value of a resolved environment variable

**Usage:** `projects get-resolved-environment-variable-value <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Optional environment ID | - | No |
| `--var-id <n>` | Exact manual environment-variable row ID | - | No |
| `--service-id <n>` | Integration service ID shown by the resolved list | - | No |
| `--json` | Output in JSON format | - | No |

### `projects get-environment-variable`

Get one environment variable of a project by its ID

**Usage:** `projects get-environment-variable <var-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects dismiss-environment-variable-secret-suggestion`

Dismiss the "store as secret" suggestion for a regular variable

**Usage:** `projects dismiss-environment-variable-secret-suggestion <var-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `projects get-environment`

Get a specific environment by ID or slug

**Usage:** `projects get-environment <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects get-environment-domains`

Get all environment domains for a specific environment

**Usage:** `projects get-environment-domains <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects add-environment-domain`

Add a new environment domain

**Usage:** `projects add-environment-domain <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--domain <value>` | Body field domain [required] | - | No |
| `--is-primary` | Body field is_primary [required] | - | No |
| `--no-is-primary` | Set is_primary to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects delete-environment-domain`

Delete an environment domain

**Usage:** `projects delete-environment-domain <env-id> <domain-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `projects update-environment-subdomain`

Rename the auto-managed subdomain for an environment

**Usage:** `projects update-environment-subdomain <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--subdomain <value>` | New subdomain label [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects teardown-environment`

Teardown an environment and all its active deployments

**Usage:** `projects teardown-environment <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `projects reinstall-github-webhook`

Install or reinstall the GitHub repository webhook for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects reinstall-gitlab-webhook`

Reinstall the GitLab webhook for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects get-ingest-setup`

Get where this project's apps send telemetry

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Attribute telemetry to this environment of the project | - | No |
| `--json` | Output in JSON format | - | No |

### `projects update-service-template-runtime`

Update service template runtime

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--command <value>` | Empty means use the image's own default command (repeatable) | - | No |
| `--cpu-limit <n>` | Body field cpuLimit | - | No |
| `--cpu-request <n>` | Body field cpuRequest | - | No |
| `--exposed-port <n>` | Body field exposedPort | - | No |
| `--health-check-path <value>` | Body field healthCheckPath [required] | - | No |
| `--image-ref <value>` | Body field imageRef [required] | - | No |
| `--memory-limit <n>` | Body field memoryLimit | - | No |
| `--memory-request <n>` | Body field memoryRequest | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects get-service-template`

Return the immutable service-template release applied to a project together with catalog drift, missing..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `projects upgrade-service-template`

Upgrade project service template

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-variables <json>` | Values for inputs introduced by the target release (JSON) | - | No |
| `--target-version <value>` | Optimistic target selected from the preview [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

## `drop`

Detect and deploy a local source directory or ZIP without Git

**Usage:** `drop <path> [options]`

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

**Usage:** `deploy [project] [options]`

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
| `--image <image>` | Docker image reference (e.g., ghcr.io/org/app:v1.0) | - | No |
| `--external-image-id <id>` | Deploy an image already registered with Temps instead of --image | - | No |
| `--claim-local` | --image is on the platform host's Docker daemon, not in a registry: claim it | - | No |
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
| `--build-command <cmd>` | With no Dockerfile: the build command in the one generated from the project's preset | - | No |
| `--install-command <cmd>` | With no Dockerfile: the install command in the generated one | - | No |
| `--output-dir <dir>` | With no Dockerfile: the static output directory in the generated one | - | No |
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
- `container-logs` - Show live container logs, including retained failed deployments
- `failure-report` - Preview or send a redacted deploy-failure trace
- `get-activity-graph` - Get deployment activity graph showing daily deployment counts Similar to GitHub's contribution graph
- `get-latest-media` - Get the latest deployment URL and screenshot location for multiple projects
- `list-managed-environment-variables` - List the environment variables generated by Temps for a deployment preset
- `metrics` - Metrics commands
- `compose` - Compose commands
- `purge-asset-cache` - Purge all cached static assets for a project
- `list-container-logs` - List the captured (historical) container-log dumps for a deployment
- `get-container-log-content` - Get the captured text content of a single historical container-log dump
- `tail-job-logs` - Tail logs for a specific deployment job in real-time via WebSocket
- `get-operations` - Get all operations for a deployment
- `execute-operation` - Execute a deployment operation (deploy, mark_complete, take_screenshot)
- `get-operation-status` - Get the status of a specific operation type
- `promote` - Promote a deployment to another environment
- `purge-environment-asset-cache` - Purge cached static assets for a specific environment
- `get-container-logs` - Get logs for a container in an environment via WebSocket
- `get-container-logs-by-id` - Get logs for a specific container by container ID via WebSocket
- `deploy-from-image-upload` - Deploy from an uploaded Docker image tarball
- `images` - Images commands
- `static-bundles` - Static bundles commands

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
| `-p, --project <project>` | Project slug or ID (defaults to the linked project) | - | No |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `deployments rollback`

Rollback to previous deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID (defaults to the linked project) | - | No |
| `-e, --environment <env>` | Target environment | `production` | No |
| `--to <deployment>` | Rollback to specific deployment ID | - | No |
| `-y, --yes` | Skip confirmation | - | No |

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

### `deployments container-logs`

Show live container logs, including retained failed deployments

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment | `production` | No |
| `-d, --deployment <id>` | Deployment ID | - | Yes |
| `-c, --container <id>` | Container ID or name (partial match supported) | - | No |
| `-n, --tail <lines>` | Number of lines to tail | `1000` | No |
| `-t, --timestamps` | Show timestamps | - | No |
| `-f, --follow` | Follow log output | - | No |

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

### `deployments get-activity-graph`

Get deployment activity graph showing daily deployment counts Similar to GitHub's contribution graph

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter by project ID (optional) | - | No |
| `--environment-id <n>` | Filter by environment ID (optional) | - | No |
| `--days <n>` | Number of days to include (default: 365) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments get-latest-media`

Get the latest deployment URL and screenshot location for multiple projects

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-ids <value>` | Comma-separated project IDs [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments list-managed-environment-variables`

List the environment variables generated by Temps for a deployment preset

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--preset <value>` | Framework preset used to select public browser variable names [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments metrics`

Metrics commands

**Subcommands:**

- `get-range` - Fetch a time-series range for a single metric on a deployment
- `toggle` - Enable or disable OTLP metric ingestion for a deployment
- `get-latest` - Fetch the most-recent metric values for a deployment

#### `deployments metrics get-range`

Fetch a time-series range for a single metric on a deployment

**Usage:** `deployments metrics get-range <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--metric <value>` | Metric name, e.g. '"pg.connections_active"' [required] | - | No |
| `--range <value>` | Time window: '"1h"' \| '"6h"' \| '"24h"' \| '"7d"' | - | No |
| `--percentile <number>` | Optional histogram percentile (0–100) | - | No |
| `--start-time <value>` | Explicit window start (ISO 8601) | - | No |
| `--end-time <value>` | Explicit window end (ISO 8601) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments metrics toggle`

Enable or disable OTLP metric ingestion for a deployment

**Usage:** `deployments metrics toggle <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enabled` | Whether to enable ('true') or disable ('false') metric ingestion [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--path <value>` | Prometheus scrape path (optional, defaults to '/metrics') | - | No |
| `--port <n>` | Prometheus scrape port (optional) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments metrics get-latest`

Fetch the most-recent metric values for a deployment

**Usage:** `deployments metrics get-latest <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `deployments compose`

Compose commands

**Subcommands:**

- `get-status` - The host's Docker Compose CLI, as probed at startup
- `get-validation` - The project's most recent stored Compose check (from its last deployment or the revalidation after an..
- `run-check` - Run the deploy job's Compose configuration check for a deployment, without deploying: the deployment's..
- `get-placement` - Where this environment's Compose stack lives: its home node, a pending move, and whether the environment's..
- `move-stack` - Move this environment's Compose stack to another node

#### `deployments compose get-status`

The host's Docker Compose CLI, as probed at startup

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `deployments compose get-validation`

The project's most recent stored Compose check (from its last deployment or the revalidation after an..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments compose run-check`

Run the deploy job's Compose configuration check for a deployment, without deploying: the deployment's..

**Usage:** `deployments compose run-check <deployment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments compose get-placement`

Where this environment's Compose stack lives: its home node, a pending move, and whether the environment's..

**Usage:** `deployments compose get-placement <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments compose move-stack`

Move this environment's Compose stack to another node

**Usage:** `deployments compose move-stack <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--acknowledge-data-loss` | The stack starts on the new node with empty volumes; the data on its current home is kept there but not copied | - | No |
| `--no-acknowledge-data-loss` | Set acknowledge_data_loss to false | - | No |
| `--node-id <n>` | The node to move the stack to; '0' is the control plane [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments purge-asset-cache`

Purge all cached static assets for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments list-container-logs`

List the captured (historical) container-log dumps for a deployment

**Usage:** `deployments list-container-logs <deployment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments get-container-log-content`

Get the captured text content of a single historical container-log dump

**Usage:** `deployments get-container-log-content <deployment-id> <log-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments tail-job-logs`

Tail logs for a specific deployment job in real-time via WebSocket

**Usage:** `deployments tail-job-logs <deployment-id> <job-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments get-operations`

Get all operations for a deployment

**Usage:** `deployments get-operations <deployment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments execute-operation`

Execute a deployment operation (deploy, mark_complete, take_screenshot)

**Usage:** `deployments execute-operation <deployment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--operation <value>` | Body field operation [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments get-operation-status`

Get the status of a specific operation type

**Usage:** `deployments get-operation-status <deployment-id> <operation-type> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments promote`

Promote a deployment to another environment

**Usage:** `deployments promote <deployment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--target-environment-id <n>` | Target environment ID to promote the deployment to [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments purge-environment-asset-cache`

Purge cached static assets for a specific environment

**Usage:** `deployments purge-environment-asset-cache <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments get-container-logs`

Get logs for a container in an environment via WebSocket

**Usage:** `deployments get-container-logs <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <n>` | Start date for logs | - | No |
| `--end-date <n>` | End date for logs | - | No |
| `--tail <value>` | Number of lines to tail (or 'all') | - | No |
| `--container-name <value>` | Optional container name (defaults to first/primary container) | - | No |
| `--timestamps` | Include timestamps in log output (default: false) | - | No |
| `--no-timestamps` | Set timestamps to false | - | No |
| `--follow` | Follow log output in real-time (default: true) | - | No |
| `--no-follow` | Set follow to false | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments get-container-logs-by-id`

Get logs for a specific container by container ID via WebSocket

**Usage:** `deployments get-container-logs-by-id <environment-id> <container-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <n>` | Start date for logs | - | No |
| `--end-date <n>` | End date for logs | - | No |
| `--tail <value>` | Number of lines to tail (or 'all') | - | No |
| `--timestamps` | Include timestamps in log output (default: false) | - | No |
| `--no-timestamps` | Set timestamps to false | - | No |
| `--follow` | Follow log output in real-time (default: true) | - | No |
| `--no-follow` | Set follow to false | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments deploy-from-image-upload`

Deploy from an uploaded Docker image tarball

**Usage:** `deployments deploy-from-image-upload <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--tag <value>` | Deprecated display hint retained for wire compatibility | - | No |
| `--health-check-path <value>` | Optional HTTP health-check path override (e.g. "/api/healthz") | - | No |
| `--upload-request-id <value>` | Client-generated UUID identifying this upload attempt | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments images`

Images commands

**Subcommands:**

- `list-remote-external` - List external images for a project
- `register-external` - Register an external Docker image
- `delete-external` - Delete an external image
- `get-remote-external` - Get details of a specific external image
- `list-external` - List all external images for a project
- `push-external` - Push an external Docker image
- `get-external` - Get details of a specific external image

#### `deployments images list-remote-external`

List external images for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments images register-external`

Register an external Docker image

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--digest <value>` | Image digest (sha256:...) | - | No |
| `--image-ref <value>` | Docker image reference (e.g., "ghcr.io/org/app:v1.0") [required] | - | No |
| `--metadata <json>` | Additional metadata (JSON) | - | No |
| `--tag <value>` | Image tag | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments images delete-external`

Delete an external image

**Usage:** `deployments images delete-external <image-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments images get-remote-external`

Get details of a specific external image

**Usage:** `deployments images get-remote-external <image-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments images list-external`

List all external images for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments images push-external`

Push an external Docker image

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--image-ref <value>` | Body field image_ref [required] | - | No |
| `--metadata <json>` | Body field metadata (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments images get-external`

Get details of a specific external image

**Usage:** `deployments images get-external <image-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments static-bundles`

Static bundles commands

**Subcommands:**

- `list` - List static bundles for a project
- `delete` - Delete a static bundle
- `get` - Get details of a specific static bundle

#### `deployments static-bundles list`

List static bundles for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20) | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments static-bundles delete`

Delete a static bundle

**Usage:** `deployments static-bundles delete <bundle-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `deployments static-bundles get`

Get details of a specific static bundle

**Usage:** `deployments static-bundles get <bundle-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

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
- `get-by-host` - Get domain details by hostname
- `get-on-demand-cert-status` - Get on-demand TLS certificate status for a hostname
- `list-on-demand-certs` - List on-demand TLS certificate attempts
- `get-certificate-settings` - Get certificate settings
- `update-certificate-settings` - Update certificate settings
- `get-by-id` - Get domain by ID
- `get-challenge-token` - Get challenge token for a domain (returns plain text token)

### `domains list` (alias: `ls`)

List domains

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--search <value>` | Only domains whose name contains this | - | No |
| `--sort-by <value>` | Sort column (default created_at) (one of: created_at, domain, status, expiration) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

### `domains add`

Add a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `-c, --challenge <type>` | Challenge type (http-01 or dns-01) | `http-01` | No |
| `--manual-dns` | With dns-01, print the TXT record to add by hand even when a DNS provider in Temps manages the zone | - | No |
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
| `--sort-by <value>` | Sort column (default created_at) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

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

### `domains get-by-host`

Get domain details by hostname

**Usage:** `domains get-by-host <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains get-on-demand-cert-status`

Get on-demand TLS certificate status for a hostname

**Usage:** `domains get-on-demand-cert-status <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains list-on-demand-certs`

List on-demand TLS certificate attempts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of items per page (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `domains get-certificate-settings`

Get certificate settings

**Usage:** `domains get-certificate-settings <domain-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains update-certificate-settings`

Update certificate settings

**Usage:** `domains update-certificate-settings <domain-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--alert-days <n>` | Override the instance SSL expiry warning threshold; null inherits it | - | No |
| `--alerts-enabled` | Override instance SSL expiry alerts; null inherits it | - | No |
| `--no-alerts-enabled` | Set alerts_enabled to false | - | No |
| `--challenge-type <value>` | Validation method for future ACME orders [required] | - | No |
| `--renewal-days <n>` | Start ACME renewal this many days before expiry (1–90) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `domains get-by-id`

Get domain by ID

**Usage:** `domains get-by-id <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains get-challenge-token`

Get challenge token for a domain (returns plain text token)

**Usage:** `domains get-challenge-token <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
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
- `placement` - Where the scheduler would run an environment's replicas, and why
- `isolation` - How an environment's replicas are isolated: a container (process) or a Firecracker microVM (micro_vm). Honoured only for environments whose deployments run on the workload plane
- `volumes` - Persistent volumes mounted into an environment's replicas; a replica that mounts one is pinned to its node
- `ingress` - Whether visitors must sign in to Temps to open an environment
- `crons` - Manage cron jobs
- `reachability` - Reachability commands
- `create-ingress-login-grant` - Create ingress login grant
- `sleep` - Sleep an on-demand environment
- `wake` - Wake a sleeping on-demand environment
- `health` - Health commands
- `settings` - Change any setting of one environment (by its numeric ID); flags left out keep their value. `resources`, `timeouts` and `force-https` cover the common ones by name

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

**Usage:** `environments delete <environment> [options]`

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
- `set` - Set an environment variable. For a secret, read the value from a file or stdin (--value-file, --value-stdin) so it stays out of shell history and `ps`; the CLI never prints it
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

**Usage:** `environments vars get <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Specify environment (if variable exists in multiple) | - | No |

#### `environments vars set`

Set an environment variable. For a secret, read the value from a file or stdin (--value-file, --value-stdin) so it stays out of shell history and `ps`; the CLI never prints it

**Usage:** `environments vars set <key> [value] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names (interactive if not provided) | - | No |
| `--preview` | Also include in current and future preview environments | - | No |
| `--no-preview` | Exclude from preview environments | - | No |
| `--update` | Update existing variable instead of creating new | - | No |
| `--secret` | Store as a secret: the value is masked in the UI and never returned by the API. One-way — to make a secret readable again you must delete the variable and create it anew | - | No |
| `--value-file <path>` | Read the value from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the value from stdin, e.g. piped from a secret manager | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `environments vars delete` (alias: `rm`, `unset`)

Delete an environment variable

**Usage:** `environments vars delete <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Delete only from specific environment | - | No |
| `-f, --force` | Skip confirmation | - | No |

#### `environments vars import`

Import environment variables from a .env file

**Usage:** `environments vars import [file] [options]`

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

**Usage:** `environments resources <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--cpu <millicores>` | CPU limit in millicores (1000 = 1 core, e.g., 500 = 0.5 CPU) | - | No |
| `--memory <mb>` | Memory limit in MB (e.g., 512) | - | No |
| `--cpu-request <millicores>` | CPU request in millicores (recorded; not currently enforced) | - | No |
| `--memory-request <mb>` | Memory request in MB (recorded; not currently enforced) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments timeouts`

View or set upstream request/idle timeouts for an environment

**Usage:** `environments timeouts <environment> [options]`

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

**Usage:** `environments force-https <environment> [options]`

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
| `--load-balancing <json>` | Replica routing policy as JSON; null restores project inheritance | - | No |
| `--json` | Output in JSON format | - | No |

### `environments placement`

Where the scheduler would run an environment's replicas, and why

**Subcommands:**

- `explain` - Show the nodes each replica would run on now, the eligible nodes ranked, and the ones excluded with the reason
- `recommend` - Recommend target nodes from where the environment actually ran (recommended, never applied)

#### `environments placement explain`

Show the nodes each replica would run on now, the eligible nodes ranked, and the ones excluded with the reason

**Usage:** `environments placement explain <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments placement recommend`

Recommend target nodes from where the environment actually ran (recommended, never applied)

**Usage:** `environments placement recommend <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--window-days <days>` | How many days of history to consider (1-90, default 7) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments isolation`

How an environment's replicas are isolated: a container (process) or a Firecracker microVM (micro_vm). Honoured only for environments whose deployments run on the workload plane

**Subcommands:**

- `show` - Show the isolation and whether it takes effect
- `set` - Set the isolation: process or micro_vm (a microVM replica only runs on a node that provides one)

#### `environments isolation show`

Show the isolation and whether it takes effect

**Usage:** `environments isolation show <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments isolation set`

Set the isolation: process or micro_vm (a microVM replica only runs on a node that provides one)

**Usage:** `environments isolation set <environment> <isolation> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

### `environments volumes`

Persistent volumes mounted into an environment's replicas; a replica that mounts one is pinned to its node

**Subcommands:**

- `list` (`ls`) - List the environment's volumes, newest first
- `create` - Create a volume (attach it to mount it)
- `status` - Show whether the next deploy mounts the attached volumes, and what to fix if not
- `attach` - Mount a volume (id or name) into every replica at --mount-path
- `detach` - Stop mounting a volume (id or name); its data is kept
- `rm` (`delete`) - Delete a volume (id or name), and its data unless it was created with --retain

#### `environments volumes list` (alias: `ls`)

List the environment's volumes, newest first

**Usage:** `environments volumes list <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--page <page>` | Page number (default 1) | - | No |
| `--page-size <size>` | Results per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes create`

Create a volume (attach it to mount it)

**Usage:** `environments volumes create <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-n, --name <name>` | Volume name: 2-64 characters of a-z, 0-9, "_", "." or "-" | - | Yes |
| `--size-mb <mb>` | Size in MB, when the backend supports a size | - | No |
| `--retain` | Keep the data when the volume is deleted | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes status`

Show whether the next deploy mounts the attached volumes, and what to fix if not

**Usage:** `environments volumes status <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes attach`

Mount a volume (id or name) into every replica at --mount-path

**Usage:** `environments volumes attach <environment> <volume> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--mount-path <path>` | Absolute path inside each replica | - | Yes |
| `--read-only` | Mount it read-only | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes detach`

Stop mounting a volume (id or name); its data is kept

**Usage:** `environments volumes detach <environment> <volume> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes rm` (alias: `delete`)

Delete a volume (id or name), and its data unless it was created with --retain

**Usage:** `environments volumes rm <environment> <volume> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments ingress`

Whether visitors must sign in to Temps to open an environment

**Subcommands:**

- `show` - Show the login requirement and whether the proxy enforces it now
- `set` - Require visitors to sign in to Temps, or stop requiring it
- `delete-connector` - Delete a tunnel at the provider and unpublish every hostname it carries
- `get-connector` - Get one tunnel
- `retry-hostname-dns` - Retry DNS setup for a failed hostname publication on its current tunnel
- `list-providers` - List the tunnel providers this control plane offers

#### `environments ingress show`

Show the login requirement and whether the proxy enforces it now

**Usage:** `environments ingress show <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments ingress set`

Require visitors to sign in to Temps, or stop requiring it

**Usage:** `environments ingress set <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--require-login` | Require sign-in only when ingress mode is enforce. Check login_enforced in JSON output or Enforced now in text output | - | No |
| `--no-require-login` | Visitors do not need to sign in | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments ingress delete-connector`

Delete a tunnel at the provider and unpublish every hostname it carries

**Usage:** `environments ingress delete-connector <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments ingress get-connector`

Get one tunnel

**Usage:** `environments ingress get-connector <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments ingress retry-hostname-dns`

Retry DNS setup for a failed hostname publication on its current tunnel

**Usage:** `environments ingress retry-hostname-dns <id> <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments ingress list-providers`

List the tunnel providers this control plane offers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
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

### `environments reachability`

Reachability commands

**Subcommands:**

- `get-certificate-methods` - Which ways to give a domain HTTPS work here
- `check-cloudflare-token` - Check which permissions a pasted Cloudflare API token has
- `get-connector-hosts` - What a tunnel publishes for environments, or would once turned on
- `set-connector-hosts` - Turn a tunnel's environment hosts on or off
- `get` - Whether visitors can reach each address of an environment
- `apply` - Carry out one option of a stored plan
- `get-choices` - Which audiences, and which ways in, can work for an environment
- `plan` - What *Make it reachable* would do for an address
- `list-runs` - The newest runs of an environment
- `get-run` - One run, with the progress of each step and what it created
- `decide-conflict` - Answer a conflict: a DNS record someone else changed while the run was setting it, or after it did (ADR..
- `resume-run` - Carry on a waiting or interrupted run from its first step not done
- `undo-run` - Remove exactly what a run created, newest first
- `get-preview-domain-status` - Whether the preview domain can be reached

#### `environments reachability get-certificate-methods`

Which ways to give a domain HTTPS work here

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain <value>` | The domain to judge, such as 'my-app.example.com' or '*.example.com' | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability check-cloudflare-token`

Check which permissions a pasted Cloudflare API token has

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--hostname <value>` | A hostname the token is meant to serve, when the zone is not known: the longest zone the token reaches... | - | No |
| `--token-stdin` | The token, as pasted [required] (read from stdin) | - | No |
| `--token-file <path>` | The token, as pasted [required] (read from a file) | - | No |
| `--zone <value>` | The zone the token is meant for | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get-connector-hosts`

What a tunnel publishes for environments, or would once turned on

**Usage:** `environments reachability get-connector-hosts <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments reachability set-connector-hosts`

Turn a tunnel's environment hosts on or off

**Usage:** `environments reachability set-connector-hosts <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--confirm <value>` | The 'confirm_token' of the list that was shown and accepted: turning on publishes every host in it,... | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get`

Whether visitors can reach each address of an environment

**Usage:** `environments reachability get <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability apply`

Carry out one option of a stored plan

**Usage:** `environments reachability apply <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--answers <json>` | Answers to the option's fields by key. 'address', when given, must be the plan's; 'dns_provider_id' takes... (JSON) | - | No |
| `--confirmed` | The user saw and accepted the option's 'confirmation' | - | No |
| `--no-confirmed` | Set confirmed to false | - | No |
| `--option <value>` | Which option of the plan to carry out (one of: existing_tunnel, direct_managed_dns, new_tunnel, new_tunnel_with_token, direct_manual_dns, private_worker, tailscale_serve, tailscale_service, tailscale_service_with_credential, tunnel_environment_hosts) [required] | - | No |
| `--plan-id <value>` | The plan this acts on, from 'POST …/reachability/plan' [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get-choices`

Which audiences, and which ways in, can work for an environment

**Usage:** `environments reachability get-choices <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability plan`

What *Make it reachable* would do for an address

**Usage:** `environments reachability plan <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--address <value>` | The hostname to make reachable; Temps suggests one without it | - | No |
| `--audience <value>` | Who an address is for (one of: public, private, signed_in, tailnet_users, unsupported) | - | No |
| `--gate <value>` | Which gate an address has (D7) (one of: temps, cloudflare_access, unsupported) | - | No |
| `--principals <json>` | For 'tailnet_users': the tailnet logins and tags it admits (ADR 065 D6) (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability list-runs`

The newest runs of an environment

**Usage:** `environments reachability list-runs <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get-run`

One run, with the progress of each step and what it created

**Usage:** `environments reachability get-run <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability decide-conflict`

Answer a conflict: a DNS record someone else changed while the run was setting it, or after it did (ADR..

**Usage:** `environments reachability decide-conflict <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--decision <value>` | A user's answer to a conflict (ADR 064 D4), journaled with the entry (one of: keep, overwrite) [required] | - | No |
| `--key <value>` | The journal entry in conflict [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability resume-run`

Carry on a waiting or interrupted run from its first step not done

**Usage:** `environments reachability resume-run <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--answers <json>` | 'cloudflare_api_token', when the run had not saved the account yet (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability undo-run`

Remove exactly what a run created, newest first

**Usage:** `environments reachability undo-run <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get-preview-domain-status`

Whether the preview domain can be reached

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `environments create-ingress-login-grant`

Create ingress login grant

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--host <value>` | The hostname, without a port [required] | - | No |
| `--next <value>` | Path on the host to return to | - | No |
| `--owner-id <n>` | Body field owner_id [required] | - | No |
| `--owner-kind <value>` | 'environment' or 'sandbox' (a sandbox preview host) [required] | - | No |
| `--port <n>` | The port the visitor reached the host on, when the proxy's request carries one (it leaves out the scheme's... | - | No |
| `--scheme <value>` | 'http' or 'https': how the visitor reached the host [required] | - | No |
| `--sig <value>` | Body field sig [required] | - | No |
| `--ts <n>` | Unix seconds the proxy signed the request at [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments sleep`

Sleep an on-demand environment

**Usage:** `environments sleep <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments wake`

Wake a sleeping on-demand environment

**Usage:** `environments wake <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments health`

Health commands

**Subcommands:**

- `get-runtime` - Get runtime health
- `update-runtime` - Update runtime health
- `get-project-runtime-health` - Get project runtime health

#### `environments health get-runtime`

Get runtime health

**Usage:** `environments health get-runtime <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments health update-runtime`

Update runtime health

**Usage:** `environments health update-runtime <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--paused` | Body field paused [required] | - | No |
| `--no-paused` | Set paused to false | - | No |
| `--required-healthy <n>` | Body field required_healthy | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments health get-project-runtime-health`

Get project runtime health

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments settings`

Change any setting of one environment (by its numeric ID); flags left out keep their value. `resources`, `timeouts` and `force-https` cover the common ones by name

**Usage:** `environments settings <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--branch <value>` | Branch this environment deploys | - | No |
| `--automatic-deploy` | Deploy automatically on push | - | No |
| `--no-automatic-deploy` | Set automatic_deploy to false | - | No |
| `--protected` | Never deploy this environment automatically from a push | - | No |
| `--no-protected` | Set protected to false | - | No |
| `--replicas <n>` | Container replicas | - | No |
| `--cpu-limit <n>` | CPU limit per container, in microcores | - | No |
| `--cpu-request <n>` | CPU reserved per container, in microcores | - | No |
| `--memory-limit <n>` | Memory limit per container, in MB | - | No |
| `--memory-request <n>` | Memory reserved per container, in MB | - | No |
| `--exposed-port <n>` | Port the container listens on (overrides the project's) | - | No |
| `--anti-affinity` | Spread replicas across different nodes | - | No |
| `--no-anti-affinity` | Set anti_affinity to false | - | No |
| `--target-nodes <n>` | Only deploy to this node ID (repeatable) (repeatable) | - | No |
| `--target-labels <json>` | Only deploy to nodes with these labels, as JSON, e.g. {"region":"eu"} (JSON) | - | No |
| `--build-location <value>` | Where a source deployment's image is built (one of: control_plane, node, ci) | - | No |
| `--cross-architecture-builds` | Build one image per architecture the eligible nodes run | - | No |
| `--no-cross-architecture-builds` | Set cross_architecture_builds to false | - | No |
| `--on-demand` | Start containers on the first request and stop them when idle | - | No |
| `--no-on-demand` | Set on_demand to false | - | No |
| `--idle-timeout-seconds <n>` | Idle seconds (60-86400) before an on-demand environment stops | - | No |
| `--wake-timeout-seconds <n>` | Seconds (5-120) to wait for an on-demand environment to wake | - | No |
| `--health <json>` | Health check settings as JSON, e.g. {"path":"/healthz"} (JSON) | - | No |
| `--health-check-timeout-seconds <n>` | Seconds (30-3600) a new deployment's containers get to become ready | - | No |
| `--stop-timeout-seconds <n>` | Seconds (1-3600) the previous deployment gets to finish in-flight requests | - | No |
| `--load-balancing <json>` | Load balancing settings as JSON (JSON) | - | No |
| `--max-concurrent-connections <n>` | Cap on concurrent requests to this environment's upstream (0 = unlimited) | - | No |
| `--request-timeout-seconds <n>` | HTTP request timeout, in seconds | - | No |
| `--sse-idle-timeout-seconds <n>` | Idle timeout of SSE streams, in seconds | - | No |
| `--websocket-idle-timeout-seconds <n>` | Idle timeout of WebSocket connections, in seconds | - | No |
| `--force-https` | Redirect HTTP to HTTPS | - | No |
| `--no-force-https` | Set force_https to false | - | No |
| `--password-stdin` | Password visitors must enter to open the environment (read from stdin) | - | No |
| `--password-file <path>` | Password visitors must enter to open the environment (read from a file) | - | No |
| `--security <json>` | Security settings (headers, rate limits) as JSON (JSON) | - | No |
| `--attack-mode` | Ask visitors to solve a CAPTCHA | - | No |
| `--no-attack-mode` | Set attack_mode to false | - | No |
| `--shield-mode <value>` | How Shield acts on its verdicts (one of: off, log, selective, attack) | - | No |
| `--shield-exempt-paths <value>` | Path Shield never challenges, e.g. /api/* (repeatable; replaces the list) (repeatable) | - | No |
| `--performance-metrics-enabled` | Collect web performance metrics | - | No |
| `--no-performance-metrics-enabled` | Set performance_metrics_enabled to false | - | No |
| `--session-recording-enabled` | Record visitor sessions | - | No |
| `--no-session-recording-enabled` | Set session_recording_enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
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
- `public-repos` - Public repos commands
- `repos` - Repos commands

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
| `--token-file <path>` | Read the access token from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the access token from stdin instead of --token | - | No |
| `--password-file <path>` | Read the Bitbucket app password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the Bitbucket app password from stdin instead of --password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
- `complete-connection-reauthorization` - Complete connection reauthorization
- `reauthorize-connection` - Reauthorize connection
- `create` - Create a new git provider configuration
- `start-github-manifest-flow` - Start creating a GitHub App from a manifest
- `create-gitlab-oauth` - Create a GitLab OAuth provider
- `update-credentials` - Partially update credentials for an existing git provider
- `list-repositories-by` - List all repositories for a specific provider
- `get-all-repositories-by-name` - Get all repositories with same owner/name from all git providers
- `get-repository-preset-by-name` - Get repository preset by owner and name
- `get-repository-compose-services-live` - Parse a compose file's services for a connected repository, live
- `get-repository-compose-preview` - Render a redacted effective Compose preview for a connected repository
- `get-repository-env-example-live` - Detect and parse a '.env.example'-style file for a connected repository
- `get-repository-by-id` - Get repository by ID

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
| `--token-file <path>` | Read the access token from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the access token from stdin instead of --token | - | No |
| `--password-file <path>` | Read the Bitbucket app password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the Bitbucket app password from stdin instead of --password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `providers git repos`

List available repositories

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID (lists every synced repository if omitted) | - | No |
| `--json` | Output in JSON format | - | No |
| `--search <term>` | Search repositories by name | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page (max: 100) | - | No |
| `--sort <field>` | Sort by field (name, created_at, updated_at, stars) | - | No |
| `--direction <dir>` | Sort direction: asc or desc | - | No |
| `--language <lang>` | Filter by programming language | - | No |
| `--owner <owner>` | Filter by repository owner | - | No |
| `--updated-after <value>` | Only repositories updated on or after this time (RFC 3339) | - | No |
| `--preset <value>` | Only repositories detected as this preset (e.g. nextjs); not with --id | - | No |
| `--git-provider-connection-id <n>` | Only repositories of this connection; not with --id | - | No |

#### `providers git complete-connection-reauthorization`

Complete connection reauthorization

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--code <value>` | Empty when the user cancelled at the provider; the state is still consumed [required] | - | No |
| `--state <value>` | Body field state [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git reauthorize-connection`

Reauthorize connection

**Usage:** `providers git reauthorize-connection <connection-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers git create`

Create a new git provider configuration

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--provider-type <value>` | Body field provider_type [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git start-github-manifest-flow`

Start creating a GitHub App from a manifest

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers git create-gitlab-oauth`

Create a GitLab OAuth provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | Body field base_url | - | No |
| `--client-id <value>` | Body field client_id [required] | - | No |
| `--client-secret-stdin` | A credential [required] (read from stdin) | - | No |
| `--client-secret-file <path>` | A credential [required] (read from a file) | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--redirect-uri <value>` | Body field redirect_uri [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git update-credentials`

Partially update credentials for an existing git provider

**Usage:** `providers git update-credentials <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--app-id <value>` | Application ID (GitHub App integer as string; GitLab App string) | - | No |
| `--app-secret-stdin` | A credential (read from stdin) | - | No |
| `--app-secret-file <path>` | A credential (read from a file) | - | No |
| `--client-id <value>` | OAuth client ID (GitLab OAuth, GitHub App) | - | No |
| `--client-secret-stdin` | A credential (read from stdin) | - | No |
| `--client-secret-file <path>` | A credential (read from a file) | - | No |
| `--password-stdin` | A credential (read from stdin) | - | No |
| `--password-file <path>` | A credential (read from a file) | - | No |
| `--private-key-stdin` | A credential (read from stdin) | - | No |
| `--private-key-file <path>` | A credential (read from a file) | - | No |
| `--public-key <value>` | SSH public key (one OpenSSH line) of an SSH-key provider | - | No |
| `--redirect-uri <value>` | OAuth redirect URI (GitLab OAuth / GitLab App) | - | No |
| `--token-stdin` | A credential (read from stdin) | - | No |
| `--token-file <path>` | A credential (read from a file) | - | No |
| `--username <value>` | Username of a basic-auth provider (generic Git server, Bitbucket app password) | - | No |
| `--webhook-secret-stdin` | A credential (read from stdin) | - | No |
| `--webhook-secret-file <path>` | A credential (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git list-repositories-by`

List all repositories for a specific provider

**Usage:** `providers git list-repositories-by <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number for pagination | - | No |
| `--per-page <n>` | Number of items per page (max 100) | - | No |
| `--sort <value>` | Sort field (name, created_at, updated_at, stars, watchers, size, issues) | - | No |
| `--direction <value>` | Sort direction (asc, desc) | - | No |
| `--search <value>` | Search term to filter repositories | - | No |
| `--owner <value>` | Filter by repository owner | - | No |
| `--language <value>` | Filter by programming language | - | No |
| `--private` | Filter by private status (true/false) | - | No |
| `--no-private` | Set private to false | - | No |
| `--preset <value>` | Cached default-branch preset slug; __undetected__ means not inspected | - | No |
| `--updated-after <value>` | Updated on or after this RFC3339 timestamp | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-all-repositories-by-name`

Get all repositories with same owner/name from all git providers

**Usage:** `providers git get-all-repositories-by-name <owner> <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-preset-by-name`

Get repository preset by owner and name

**Usage:** `providers git get-repository-preset-by-name <owner> <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Git branch to check (defaults to repository's default branch) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-compose-services-live`

Parse a compose file's services for a connected repository, live

**Usage:** `providers git get-repository-compose-services-live <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Git branch to check (defaults to repository's default branch) | - | No |
| `--path <value>` | Compose file path to fetch and parse [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-compose-preview`

Render a redacted effective Compose preview for a connected repository

**Usage:** `providers git get-repository-compose-preview <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Body field branch | - | No |
| `--compose-override <value>` | Body field composeOverride | - | No |
| `--environment-id <n>` | Environment whose variables resolve '${VAR}'; defaults to the project's production environment | - | No |
| `--excluded-services <value>` | Body field excludedServices (repeatable) | - | No |
| `--path <value>` | Body field path [required] | - | No |
| `--preview-policy <json>` | Advisory preview only (JSON) | - | No |
| `--project-id <n>` | The project this Compose file deploys | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-env-example-live`

Detect and parse a '.env.example'-style file for a connected repository

**Usage:** `providers git get-repository-env-example-live <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Git branch to check (defaults to repository's default branch) | - | No |
| `--root-directory <value>` | Project root directory to search (defaults to repository root) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers git get-repository-by-id`

Get repository by ID

**Usage:** `providers git get-repository-by-id <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `providers connections` (alias: `conn`)

Manage Git provider connections

**Subcommands:**

- `list` (`ls`) - List all Git connections
- `get` - Show one Git connection: account, health and sync state
- `show` - Show connection details for a provider
- `delete` (`rm`) - Delete a Git connection
- `activate` - Activate a Git connection
- `deactivate` - Deactivate a Git connection
- `sync` - Sync repositories for a Git connection
- `update-token` - Update access token for a Git connection
- `validate` - Validate a Git connection
- `run-health-check` - Run an on-demand health check for a git connection
- `create-repository` - Create an empty repository through one of your git connections
- `commit-repository-files` - Commit files to a repository through one of your git connections
- `open-repository-pull-request` - Open a pull request through one of your git connections

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

#### `providers connections get`

Show one Git connection: account, health and sync state

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `--json` | Output in JSON format | - | No |

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
| `-t, --token <token>` | New access token (prefer --token-file or --token-stdin, which keep it out of shell history) | - | No |
| `--refresh-token <token>` | New refresh token, for providers whose access tokens expire (prefer --refresh-token-file or --refresh-token-stdin) | - | No |
| `--token-file <path>` | Read the new access token from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the new access token from stdin instead of --token | - | No |
| `--refresh-token-file <path>` | Read the new refresh token from a file (- reads stdin) instead of --refresh-token | - | No |
| `--refresh-token-stdin` | Read the new refresh token from stdin instead of --refresh-token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `providers connections validate`

Validate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `providers connections run-health-check`

Run an on-demand health check for a git connection

**Usage:** `providers connections run-health-check <connection-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers connections create-repository`

Create an empty repository through one of your git connections

**Usage:** `providers connections create-repository <connection-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--description <value>` | Short description, up to 350 characters | - | No |
| `--name <value>` | Repository name: letters, digits, '-', '_' and '.', up to 100 characters [required] | - | No |
| `--owner <value>` | Account or organization to create it under | - | No |
| `--private` | Whether the repository is private | - | No |
| `--no-private` | Set private to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers connections commit-repository-files`

Commit files to a repository through one of your git connections

**Usage:** `providers connections commit-repository-files <connection-id> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--branch <value>` | Branch to commit to | - | No |
| `--files <json>` | Files to add or overwrite (1-100, at most 5 MiB in total) (JSON) [required] | - | No |
| `--message <value>` | Commit message (at most 500 bytes) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers connections open-repository-pull-request`

Open a pull request through one of your git connections

**Usage:** `providers connections open-repository-pull-request <connection-id> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-branch <value>` | Branch to merge into | - | No |
| `--body-body <value>` | Body field body | - | No |
| `--head-branch <value>` | Branch holding the changes [required] | - | No |
| `--title <value>` | Body field title [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `providers public-repos`

Public repos commands

**Subcommands:**

- `get-repository` - Get information about a public repository (supports GitHub and GitLab)
- `get-branches` - Get branches for a public repository (supports GitHub and GitLab)
- `get-compose-services` - Parse a compose file's services for a public repository (supports GitHub and GitLab)
- `get-compose-preview` - Render a redacted effective Compose preview for a public repository
- `detect-env-example` - Detect and parse a '.env.example'-style file for a public repository (supports GitHub and GitLab)

#### `providers public-repos get-repository`

Get information about a public repository (supports GitHub and GitLab)

**Usage:** `providers public-repos get-repository <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos get-branches`

Get branches for a public repository (supports GitHub and GitLab)

**Usage:** `providers public-repos get-branches <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos get-compose-services`

Parse a compose file's services for a public repository (supports GitHub and GitLab)

**Usage:** `providers public-repos get-compose-services <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--branch <value>` | Branch to read the compose file from (default: repository's default branch) | - | No |
| `--path <value>` | Compose file path to fetch and parse (from the 'compose_files' list '/preset' already returned, or a... [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos get-compose-preview`

Render a redacted effective Compose preview for a public repository

**Usage:** `providers public-repos get-compose-preview <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--branch <value>` | Body field branch | - | No |
| `--compose-override <value>` | Body field composeOverride | - | No |
| `--excluded-services <value>` | Body field excludedServices (repeatable) | - | No |
| `--path <value>` | Body field path [required] | - | No |
| `--preview-policy <json>` | Advisory preview only (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers public-repos detect-env-example`

Detect and parse a '.env.example'-style file for a public repository (supports GitHub and GitLab)

**Usage:** `providers public-repos detect-env-example <provider> <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--base-url <value>` | HTTPS/443 origin of a self-hosted GitLab instance | - | No |
| `--branch <value>` | Branch name to inspect (default: repository's default branch) | - | No |
| `--root-directory <value>` | Project root directory to search (default: repository root) | - | No |
| `--json` | Output in JSON format | - | No |

### `providers repos`

Repos commands

**Subcommands:**

- `get-repository-branches` - Get repository branches
- `get-repository-tags` - Get repository tags
- `get-branches-by-repository-id` - Get repository branches by repository ID
- `check-commit-exists` - Check if a commit exists in a repository
- `get-tags-by-repository-id` - Get repository tags by repository ID

#### `providers repos get-repository-branches`

Get repository branches

**Usage:** `providers repos get-repository-branches <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--connection-id <n>` | Git provider connection ID (required when multiple connections have the same repo) [required] | - | No |
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers repos get-repository-tags`

Get repository tags

**Usage:** `providers repos get-repository-tags <owner> <repo> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--connection-id <n>` | Git provider connection ID (required when multiple connections have the same repo) [required] | - | No |
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers repos get-branches-by-repository-id`

Get repository branches by repository ID

**Usage:** `providers repos get-branches-by-repository-id <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `providers repos check-commit-exists`

Check if a commit exists in a repository

**Usage:** `providers repos check-commit-exists <repository-id> <commit-sha> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `providers repos get-tags-by-repository-id`

Get repository tags by repository ID

**Usage:** `providers repos get-tags-by-repository-id <repository-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--fresh` | Force fetch fresh data, bypassing cache (default: false) | - | No |
| `--no-fresh` | Set fresh to false | - | No |
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
- `list-alerts` - List open backup alerts
- `get-external-service-capability` - Report whether an existing service can produce a Cloud-restorable backup
- `list-external-service` - List all backups for a specific external service (DB-only, no S3 scan)
- `list-service-schedules` - List the schedules that target a specific external service
- `test-s3-connection-preview` - Test S3 connectivity against a prospective source configuration (before creating it)
- `set-default-s3-source` - Mark an S3 source as the default
- `test-s3-source-connection` - Test connectivity to an existing S3 source using its stored credentials
- `cancel-schedule-run` - Cancel every non-terminal child backup belonging to a schedule run
- `list-schedule-run-jobs` - List the individual backup jobs for a single scheduler run
- `update-schedule` - Update a backup schedule (partial update)
- `run-schedule-now` - Immediately fan-out a run for the given schedule (Run Now)
- `list-schedule-runs` - Paginated run history for a backup schedule (one row per scheduler tick)
- `list-schedule-services` - List the external services attached to a backup schedule
- `attach-schedule-services` - Attach one or more external services to a backup schedule
- `detach-schedule-service` - Detach a single external service from a backup schedule
- `cancel` - Cancel a single in-flight backup
- `list-children` - List the external-service child backups that belong to a parent backup

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
| `-s, --schedule <cron>` | Schedule expression: six-field cron with seconds first, e.g. "0 0 2 * * *" | - | No |
| `-r, --retention <days>` | Retention period in days | - | No |
| `-d, --description <desc>` | Description | - | No |
| `--s3-source-id <id>` | S3 Source ID | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--target-all-services` | Back up every external service (the default when no --service-ids) | - | No |
| `--no-target-all-services` | Set target_all_services to false | - | No |
| `--service-ids <n>` | Back up only these external services (repeatable) (repeatable) | - | No |
| `--include-control-plane` | Also back up Temps' own database | - | No |
| `--no-include-control-plane` | Set include_control_plane to false | - | No |
| `--max-runtime-secs <n>` | Cancel a run that takes longer than this | - | No |

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
| `--use-tls` | Connect to the endpoint over TLS (default true) | - | No |
| `--no-use-tls` | Set use_tls to false | - | No |
| `--worker-credentials <json>` | Separate credentials the backup workers use, as JSON (JSON) | - | No |
| `--backing-service-id <n>` | The external service (S3/MinIO) that hosts this bucket | - | No |
| `--is-default` | Make this the default backup destination | - | No |
| `--no-is-default` | Set is_default to false | - | No |
| `--secret-key-file <path>` | Read the S3 secret access key from a file (- reads stdin) instead of --secret-key | - | No |
| `--secret-key-stdin` | Read the S3 secret access key from stdin instead of --secret-key | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
| `--use-tls` | Connect to the endpoint over TLS (default true) | - | No |
| `--no-use-tls` | Set use_tls to false | - | No |
| `--worker-credentials <json>` | Separate credentials the backup workers use, as JSON (JSON) | - | No |
| `--secret-key-file <path>` | Read the S3 secret access key from a file (- reads stdin) instead of --secret-key | - | No |
| `--secret-key-stdin` | Read the S3 secret access key from stdin instead of --secret-key | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
| `--include-s3-scan` | Also list backups found in the bucket that this server has no record of | - | No |
| `--no-include-s3-scan` | Set include_s3_scan to false | - | No |

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
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
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
| `--confirm-restart` | Allow the backup to restart the database (PostgreSQL with WAL archiving off restarts once to turn it on) | - | No |

### `backups list-alerts`

List open backup alerts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups get-external-service-capability`

Report whether an existing service can produce a Cloud-restorable backup

**Usage:** `backups get-external-service-capability <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups list-external-service`

List all backups for a specific external service (DB-only, no S3 scan)

**Usage:** `backups list-external-service <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (1-based) | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `backups list-service-schedules`

List the schedules that target a specific external service

**Usage:** `backups list-service-schedules <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups test-s3-connection-preview`

Test S3 connectivity against a prospective source configuration (before creating it)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--access-key-id <value>` | Access key ID of the bucket's credentials, for example 'AKIA...' on AWS [required] | - | No |
| `--backing-service-id <n>` | Managed RustFS/S3 service that supplies this destination | - | No |
| `--bucket-name <value>` | Body field bucket_name [required] | - | No |
| `--bucket-path <value>` | Body field bucket_path [required] | - | No |
| `--endpoint <value>` | Optional endpoint for S3-compatible services like MinIO or RustFS, with or without a scheme | - | No |
| `--force-path-style` | Whether to use path-style addressing (default: true) | - | No |
| `--no-force-path-style` | Set force_path_style to false | - | No |
| `--is-default` | When true, make this the default source (will swap out any existing default) | - | No |
| `--no-is-default` | Set is_default to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--region <value>` | Body field region [required] | - | No |
| `--secret-key-stdin` | Secret access key that goes with the access key ID [required] (read from stdin) | - | No |
| `--secret-key-file <path>` | Secret access key that goes with the access key ID [required] (read from a file) | - | No |
| `--use-tls` | Whether the endpoint is reached over TLS | - | No |
| `--no-use-tls` | Set use_tls to false | - | No |
| `--worker-credentials <value>` | What a worker node may receive from an S3 source (ADR-102) (one of: scoped, allow_full) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `backups set-default-s3-source`

Mark an S3 source as the default

**Usage:** `backups set-default-s3-source <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups test-s3-source-connection`

Test connectivity to an existing S3 source using its stored credentials

**Usage:** `backups test-s3-source-connection <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups cancel-schedule-run`

Cancel every non-terminal child backup belonging to a schedule run

**Usage:** `backups cancel-schedule-run <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `backups list-schedule-run-jobs`

List the individual backup jobs for a single scheduler run

**Usage:** `backups list-schedule-run-jobs <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups update-schedule`

Update a backup schedule (partial update)

**Usage:** `backups update-schedule <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--description <value>` | New human-readable description | - | No |
| `--enabled` | Enable or disable the schedule | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--include-control-plane` | Toggle whether the control-plane backup is produced on every run | - | No |
| `--no-include-control-plane` | Set include_control_plane to false | - | No |
| `--max-runtime-secs <n>` | Per-schedule wall-clock timeout override (seconds) | - | No |
| `--name <value>` | New schedule name | - | No |
| `--retention-period <n>` | Days to retain backups produced by this schedule | - | No |
| `--schedule-expression <value>` | New cron expression with a leading seconds field ('sec min hour day-of-month month day-of-week', e.g. '0 0... | - | No |
| `--service-ids <n>` | Replace the explicit external-service selection atomically with the schedule update (repeatable) | - | No |
| `--tags <value>` | Replace the full tag list (repeatable) | - | No |
| `--target-all-services` | Toggle between "back up every database" ('true') and "back up only the explicit list" ('false') | - | No |
| `--no-target-all-services` | Set target_all_services to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `backups run-schedule-now`

Immediately fan-out a run for the given schedule (Run Now)

**Usage:** `backups run-schedule-now <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups list-schedule-runs`

Paginated run history for a backup schedule (one row per scheduler tick)

**Usage:** `backups list-schedule-runs <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (1-based, defaults to 1, clamped to 1 if < 1) | - | No |
| `--page-size <n>` | Items per page (defaults to 20, clamped to 100 if > 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `backups list-schedule-services`

List the external services attached to a backup schedule

**Usage:** `backups list-schedule-services <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups attach-schedule-services`

Attach one or more external services to a backup schedule

**Usage:** `backups attach-schedule-services <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--service-ids <n>` | External service ids to attach (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `backups detach-schedule-service`

Detach a single external service from a backup schedule

**Usage:** `backups detach-schedule-service <id> <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `backups cancel`

Cancel a single in-flight backup

**Usage:** `backups cancel <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `backups list-children`

List the external-service child backups that belong to a parent backup

**Usage:** `backups list-children <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `runtime-logs` (alias: `rlogs`)

View runtime container logs (use -f to follow in real-time)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name | `production` | No |
| `-c, --container <id>` | Container ID (partial match supported) | - | No |
| `-d, --deployment <id>` | Deployment ID, including failed retained containers | - | No |
| `-n, --tail <lines>` | Number of lines to tail | `1000` | No |
| `-t, --timestamps` | Show timestamps | - | No |
| `-f, --follow` | Follow log output (stream in real-time) | - | No |

## `notifications` (alias: `notify`)

Manage notification providers (Slack, Email, Webhook, Telegram, etc.)

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
- `create-cloudflare-provider` - Create a new Cloudflare Email Sending notification provider
- `update-cloudflare-provider` - Update a Cloudflare Email Sending notification provider
- `create-email-provider` - Create a new Email notification provider
- `create-webhook-provider` - Create a new Webhook notification provider
- `update-webhook-provider` - Update a Webhook notification provider
- `reveal-provider-config` - Reveal notification provider config

### `notifications list` (alias: `ls`)

List configured notification providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--sort-by <value>` | Sort column (default created_at) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

### `notifications add`

Add a new notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Provider type (slack, email, webhook, telegram) | - | No |
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
| `--header <header>` | Request header "Name: value" (for webhook, repeatable) | - | No |
| `--bot-token <token>` | Bot token from @BotFather (for telegram) | - | No |
| `--chat-id <id>` | Chat ID, e.g. -1001234567890 or @channelname (for telegram) | - | No |
| `--topic-id <id>` | Forum topic ID (for telegram, optional) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--webhook-url-file <path>` | Read the Slack webhook URL from a file (- reads stdin) instead of --webhook-url | - | No |
| `--webhook-url-stdin` | Read the Slack webhook URL from stdin instead of --webhook-url | - | No |
| `--password-file <path>` | Read the SMTP password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the SMTP password from stdin instead of --password | - | No |
| `--bot-token-file <path>` | Read the Telegram bot token from a file (- reads stdin) instead of --bot-token | - | No |
| `--bot-token-stdin` | Read the Telegram bot token from stdin instead of --bot-token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
| `--header <header>` | Request header "Name: value" (for webhook, repeatable; replaces the stored headers) | - | No |
| `--bot-token <token>` | Bot token from @BotFather (for telegram) | - | No |
| `--chat-id <id>` | Chat ID, e.g. -1001234567890 or @channelname (for telegram) | - | No |
| `--topic-id <id>` | Forum topic ID (for telegram; empty string clears it) | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompts | - | No |
| `--webhook-url-file <path>` | Read the Slack webhook URL from a file (- reads stdin) instead of --webhook-url | - | No |
| `--webhook-url-stdin` | Read the Slack webhook URL from stdin instead of --webhook-url | - | No |
| `--password-file <path>` | Read the SMTP password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the SMTP password from stdin instead of --password | - | No |
| `--bot-token-file <path>` | Read the Telegram bot token from a file (- reads stdin) instead of --bot-token | - | No |
| `--bot-token-stdin` | Read the Telegram bot token from stdin instead of --bot-token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
- `get-delivery-coverage` - Check whether a notification of the given severity would be delivered
- `test` - Send a test notification through a route to each of its providers

#### `notifications routes list` (alias: `ls`)

List notification routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--sort-by <value>` | Sort column (default created_at) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

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

#### `notifications routes get-delivery-coverage`

Check whether a notification of the given severity would be delivered

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--severity <value>` | Severity of the notification to check: debug, info, warning, error, critical or emergency [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `notifications routes test`

Send a test notification through a route to each of its providers

**Usage:** `notifications routes test <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `notifications create-cloudflare-provider`

Create a new Cloudflare Email Sending notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Configuration for a Cloudflare Email Sending notification provider (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications update-cloudflare-provider`

Update a Cloudflare Email Sending notification provider

**Usage:** `notifications update-cloudflare-provider <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Configuration for a Cloudflare Email Sending notification provider (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications create-email-provider`

Create a new Email notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Request-side SMTP config (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications create-webhook-provider`

Create a new Webhook notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Configuration for a generic webhook notification provider (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications update-webhook-provider`

Update a Webhook notification provider

**Usage:** `notifications update-webhook-provider <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Configuration for a generic webhook notification provider (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications reveal-provider-config`

Reveal notification provider config

**Usage:** `notifications reveal-provider-config <id> <field> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `dns`

Manage DNS providers and Temps-managed DNS records

**Subcommands:**

- `list` (`ls`) - List configured DNS providers
- `add` - Add a new DNS provider
- `show` - Show DNS provider details
- `remove` (`rm`) - Remove a DNS provider
- `test` - Test DNS provider connection
- `zones` - List available zones in a DNS provider
- `records` (`record`) - Manage A, AAAA and CNAME records on managed domains (ownership-guarded: Temps only changes records it owns)

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
| `-t, --type <type>` | Provider type (cloudflare, bunny, route53, digitalocean, namecheap, gcp, azure, manual) | - | No |
| `-n, --name <name>` | Provider name | - | No |
| `-d, --description <description>` | Provider description | - | No |
| `--api-token <token>` | API token (Cloudflare, DigitalOcean; prefer --api-token-stdin to keep it out of shell history) | - | No |
| `--api-token-stdin` | Read the API token from stdin | - | No |
| `--account-id <id>` | Cloudflare account ID (optional) | - | No |
| `--access-key-id <key>` | AWS access key ID | - | No |
| `--secret-access-key <secret>` | AWS secret access key (prefer --secret-access-key-stdin to keep it out of shell history) | - | No |
| `--secret-access-key-stdin` | Read the AWS secret access key from stdin | - | No |
| `--region <region>` | AWS region | - | No |
| `--api-user <user>` | Namecheap API user | - | No |
| `--api-key <key>` | API key (Bunny, Namecheap; prefer --api-key-stdin to keep it out of shell history) | - | No |
| `--api-key-stdin` | Read the Bunny or Namecheap API key from stdin | - | No |
| `--username <username>` | Namecheap username | - | No |
| `--client-ip <ip>` | Namecheap whitelisted client IP | - | No |
| `--project-id <id>` | GCP project ID | - | No |
| `--service-account-email <email>` | GCP service account email | - | No |
| `--private-key-id <id>` | GCP private key ID | - | No |
| `--private-key <key>` | GCP private key (prefer --private-key-stdin to keep it out of shell history) | - | No |
| `--private-key-stdin` | Read the GCP private key from stdin | - | No |
| `--tenant-id <id>` | Azure tenant ID | - | No |
| `--client-id <id>` | Azure client ID | - | No |
| `--client-secret <secret>` | Azure client secret (prefer --client-secret-stdin to keep it out of shell history) | - | No |
| `--client-secret-stdin` | Read the Azure client secret from stdin | - | No |
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

### `dns records` (alias: `record`)

Manage A, AAAA and CNAME records on managed domains (ownership-guarded: Temps only changes records it owns)

**Subcommands:**

- `ownership` (`owner`) - Show whether Temps owns a DNS record and may change it
- `set` (`create`) - Create or update a Temps-owned DNS record
- `import` (`adopt`) - Adopt an existing DNS record into Temps management
- `remove` (`rm`, `delete`) - Delete a Temps-owned DNS record

#### `dns records ownership` (alias: `owner`)

Show whether Temps owns a DNS record and may change it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain under a managed zone, e.g. example.com | - | Yes |
| `--name <name>` | Record name relative to the zone ("@" for apex) | - | Yes |
| `-t, --type <type>` | Record type (A, AAAA, CNAME) | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `dns records set` (alias: `create`)

Create or update a Temps-owned DNS record

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain under a managed zone, e.g. example.com | - | Yes |
| `--name <name>` | Record name relative to the zone ("@" for apex) | - | Yes |
| `-t, --type <type>` | Record type (A, AAAA, CNAME) | - | Yes |
| `--value <value>` | Record value: IPv4 address (A), IPv6 address (AAAA) or target hostname (CNAME) | - | No |
| `--ttl <seconds>` | TTL in seconds, 60-86400; omit (or use 1) for the provider default | - | No |
| `--proxied` | Proxy through the provider CDN (Cloudflare orange cloud) | - | No |
| `--no-proxied` | Do not proxy (DNS only) | - | No |
| `-p, --project <project>` | Project slug or ID to stamp as the record owner | - | No |
| `--environment-id <id>` | Environment ID to stamp as the record owner | - | No |
| `--json` | Output in JSON format | - | No |

#### `dns records import` (alias: `adopt`)

Adopt an existing DNS record into Temps management

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain under a managed zone, e.g. example.com | - | Yes |
| `--name <name>` | Record name relative to the zone ("@" for apex) | - | Yes |
| `-t, --type <type>` | Record type (A, AAAA, CNAME) | - | Yes |
| `-p, --project <project>` | Project slug or ID to stamp as the record owner | - | No |
| `--environment-id <id>` | Environment ID to stamp as the record owner | - | No |
| `--json` | Output in JSON format | - | No |

#### `dns records remove` (alias: `rm`, `delete`)

Delete a Temps-owned DNS record

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain under a managed zone, e.g. example.com | - | Yes |
| `--name <name>` | Record name relative to the zone ("@" for apex) | - | Yes |
| `-t, --type <type>` | Record type (A, AAAA, CNAME) | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

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
- `repoint-continuous-archive-source` - Repoint a Postgres/MariaDB service's continuous archiving (WAL-G, or MariaDB's binlog shipper) to a different S3 source. Data archived before this call stays under the previous source and will no longer be verifiable or replayable once archiving points at the new one.
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
- `restore-cancel` - Cancel a restore run that has not started writing data yet (refused once it has)
- `import-data-availability` - Show whether a service can receive data imported from an external database, and what source it accepts
- `import-data` - Copy a database from an external server into a database of this service (PostgreSQL, MariaDB/MySQL, MongoDB, Redis)
- `import-data-runs` - List data imports into a service, newest first
- `import-data-run` - Show one data import: outcome, phase, timings, who started it and the transfer output
- `import-data-cancel` - Cancel a running data import (refused once the data has been copied)
- `wal-health` - Probe a PostgreSQL service's WAL / archive_command health right now (archiver failures, backlog, stale replication slots) — diagnoses "Cloud backup mirror unavailable ... check that PostgreSQL's archive_command is succeeding" warnings
- `list-available-containers` - List available Docker containers that can be imported as services
- `list-health-statuses` - Current health status for many services at once
- `get-healthcheck-defaults` - Get the instance-wide default healthcheck of managed service containers
- `update-healthcheck-defaults` - Replace the instance-wide default healthcheck of managed service containers
- `list-project-services` - List services linked to a project
- `get-environment-variables` - Get all environment variables for all services linked to a project
- `get-providers-metadata` - Get provider metadata (display names, icons, descriptions)
- `get-provider-metadata` - Get metadata for a specific provider
- `get-cluster-health` - Per-member health for a Postgres HA cluster
- `reveal-environment-variables` - Reveal a service's basic environment variables in plaintext, before it is linked to any project
- `trigger-health-check` - Run a health check for one service right now
- `get-health-status` - Persisted health status for an external service
- `get-healthcheck` - Get a managed service's healthcheck
- `update-healthcheck` - Replace a managed service's healthcheck overrides
- `apply-healthcheck` - Recreate a managed service's container to apply its healthcheck now
- `get-log-ingestion` - Get this service's own container log ingestion budget
- `update-log-ingestion` - Replace this service's container log ingestion budget
- `add-cluster-member` - Begin adding a single new member to a running cluster
- `remove-cluster-member` - Remove a single member from a running cluster
- `get-cluster-member` - Get a single cluster member's current state
- `promote-cluster-member` - Promote a replica to primary by triggering a pg_auto_failover failover
- `migrate-s3` - Copy every bucket of another managed S3 service into this one
- `reveal-parameter` - Reveal one sensitive service parameter
- `get-preview-environment-variables-masked` - Get environment variables preview with masked sensitive values
- `get-preview-environment-variable-names` - Get environment variable names preview (safe - no sensitive values)
- `issue-runtime-credentials` - Issue live connection credentials for a service in an environment
- `update-resources` - Update a service's resource limits (memory, CPU caps)
- `retry-cluster` - Retry a failed cluster service initialization
- `get-runtime` - Inspect a service's container(s): status, restart count, OOM-killed flag, exit code, and the cgroup limits..
- `get-stats` - Sample current CPU/memory usage from each of a service's containers
- `get-tls` - Get service tls
- `restored-databases` - What the latest PostgreSQL restore brought back: its databases and users, which linked project reads each, and linked databases the backup does not have
- `set-link-database` - Change which database a linked project reads from this service, for example one a restore brought back. Its apps read it from their next deployment
- `update-tls` - Update service tls
- `get-managed-ca-certificate` - Get managed ca certificate
- `test-wal-archive-target` - Test wal archive target
- `get-workload-mode` - Get a database's workload mode (internal, operator-only)
- `update-workload-mode` - Change a database's workload mode (internal, operator-only)
- `get-workload-pending-changes` - List what a recreate from the workload spec would change (internal, operator-only)
- `recreate-from-workload-spec` - Recreate a database's container from its workload spec (internal, operator-only)
- `external-reset-pg-stat-statements` - Reset all statistics accumulated by 'pg_stat_statements' for a Postgres service
- `query` - Query commands
- `pg-upgrade` - Pg upgrade commands

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
| `--backend <backend>` | S3 storage engine: rustfs (default) or garage (s3/blob only) | - | No |
| `--bucket <name>` | Bucket to create (repeatable, s3/blob only; the first is S3_BUCKET) | `` | No |
| `--node <id>` | Worker node to run the service on (default: the control plane) | - | No |
| `--topology <topology>` | standalone (default) or cluster (highly available, several members) | - | No |
| `--member <role[@node]>` | With --topology cluster: a member, monitor, primary or replica, optionally on a node id (repeatable; exactly one monitor) | `` | No |
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

**Usage:** `services types info <type> [options]`

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

### `services repoint-continuous-archive-source`

Repoint a Postgres/MariaDB service's continuous archiving (WAL-G, or MariaDB's binlog shipper) to a different S3 source. Data archived before this call stays under the previous source and will no longer be verifiable or replayable once archiving points at the new one.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--s3-source <id>` | S3 source ID to point continuous archiving at | - | Yes |

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
| `--database-mode <mode>` | Which database deployments get: project (one per project), project_environment (one per environment) or custom | - | No |
| `--database-name <name>` | With --database-mode custom: the exact database name | - | No |

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

**Usage:** `services connect <name> [options]`

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
| `--backup-id <id>` | Backup ID to restore from (see `list-backups`) | - | No |
| `--backup-location <s3-key>` | Instead of --backup-id: the S3 key of a backup this instance did not record (one found by an S3 scan, made by another Temps instance) | - | No |
| `--backup-engine <engine>` | With --backup-location: postgres, redis, mongodb or s3 | - | No |
| `--s3-source <id>` | With --backup-location: the S3 source the backup is in | - | No |
| `--new-service [name]` | Clone into a new service. Omit the value or pass "auto" to accept the auto-suggested name. | - | No |
| `--pitr <iso>` | Point-in-time recovery target, ISO 8601 timestamp (requires a PITR-capable backup). Combine with --new-service to route PITR into a new service. | - | No |
| `--confirm-cross-service` | Confirm an in-place (or PITR in-place) restore of a backup produced by a different service. Required by the server for such restores; recorded in the audit log. | - | No |
| `-y, --yes` | Skip confirmation | - | No |
| `--no-wait` | Return immediately without polling run status | - | No |
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `plan` - Plan restore

#### `services restore plan`

Plan restore

**Usage:** `services restore plan <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | Body field mode (one of: in_place, new_service, pitr) [required] | - | No |
| `--name <value>` | Name for the new service | - | No |
| `--parameter-overrides <json>` | Optional parameter overrides (port, docker_image, database) (JSON) | - | No |
| `--new-service-name <value>` | Required when 'to_new_service' is true | - | No |
| `--target <json>` | Recovery target kind + value (JSON) | - | No |
| `--to-new-service` | Whether PITR restores in place or creates a new service | - | No |
| `--no-to-new-service` | Set to_new_service to false | - | No |
| `--backup-engine <value>` | Engine of the backup when specified by 'backup_location' ("postgres", "redis", "mongodb", "s3") | - | No |
| `--backup-id <n>` | DB id of the backup to restore from | - | No |
| `--backup-location <value>` | Raw S3 URL / key of the backup — used when restoring a backup discovered by S3 scan (i.e., produced by... | - | No |
| `--confirm-cross-service` | Explicit confirmation for a destructive cross-service restore | - | No |
| `--no-confirm-cross-service` | Set confirm_cross_service to false | - | No |
| `--s3-source-id <n>` | S3 source the 'backup_location' lives in | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
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

### `services restore-cancel`

Cancel a restore run that has not started writing data yet (refused once it has)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Restore run ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services import-data-availability`

Show whether a service can receive data imported from an external database, and what source it accepts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services import-data`

Copy a database from an external server into a database of this service (PostgreSQL, MariaDB/MySQL, MongoDB, Redis)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID to import into | - | Yes |
| `--target <name>` | Database that receives the data (created if missing; for Redis, the resource name, usually <project>_<environment>) | - | Yes |
| `--source-url-env <var>` | Name of an environment variable holding the source connection string (recommended: keeps the password out of shell history) | - | No |
| `--source-url <url>` | Source connection string (visible in shell history and the process list; prefer --source-url-env) | - | No |
| `--replace` | Drop the target database first if it already holds data (DESTRUCTIVE) | - | No |
| `--confirm-target <name>` | Required with --replace when not interactive: repeat the target name | - | No |
| `--timeout <minutes>` | Stop the copy after this many minutes (server default when omitted) | - | No |
| `-y, --yes` | Skip the confirmation prompt | - | No |
| `--no-wait` | Return after starting instead of waiting for the result | - | No |
| `--json` | Output in JSON format | - | No |

### `services import-data-runs`

List data imports into a service, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--page <n>` | Page number (default 1) | - | No |
| `--page-size <n>` | Items per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `services import-data-run`

Show one data import: outcome, phase, timings, who started it and the transfer output

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--run <id>` | Data import ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services import-data-cancel`

Cancel a running data import (refused once the data has been copied)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--run <id>` | Data import ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services wal-health`

Probe a PostgreSQL service's WAL / archive_command health right now (archiver failures, backlog, stale replication slots) — diagnoses "Cloud backup mirror unavailable ... check that PostgreSQL's archive_command is succeeding" warnings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services list-available-containers`

List available Docker containers that can be imported as services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services list-health-statuses`

Current health status for many services at once

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ids <value>` | Comma-separated service IDs | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-healthcheck-defaults`

Get the instance-wide default healthcheck of managed service containers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-healthcheck-defaults`

Replace the instance-wide default healthcheck of managed service containers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--interval-secs <n>` | Seconds between checks once healthy (5–3600) | - | No |
| `--retries <n>` | Consecutive failures that make the container unhealthy (1–10) | - | No |
| `--start-interval-secs <n>` | Seconds between checks while starting (1–60) | - | No |
| `--start-period-secs <n>` | Seconds after start during which failures do not count (0–600) | - | No |
| `--timeout-secs <n>` | Seconds one check may run (1–60, and shorter than the interval) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services list-project-services`

List services linked to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of items per page (max 100) | - | No |
| `--sort-by <value>` | Query parameter sort_by | - | No |
| `--sort-order <value>` | Query parameter sort_order | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-environment-variables`

Get all environment variables for all services linked to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-providers-metadata`

Get provider metadata (display names, icons, descriptions)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-provider-metadata`

Get metadata for a specific provider

**Usage:** `services get-provider-metadata <service-type> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-cluster-health`

Per-member health for a Postgres HA cluster

**Usage:** `services get-cluster-health <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services reveal-environment-variables`

Reveal a service's basic environment variables in plaintext, before it is linked to any project

**Usage:** `services reveal-environment-variables <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services trigger-health-check`

Run a health check for one service right now

**Usage:** `services trigger-health-check <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-health-status`

Persisted health status for an external service

**Usage:** `services get-health-status <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max number of recent checks (default 50, max 200) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-healthcheck`

Get a managed service's healthcheck

**Usage:** `services get-healthcheck <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-healthcheck`

Replace a managed service's healthcheck overrides

**Usage:** `services update-healthcheck <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--interval-secs <n>` | Seconds between checks once healthy (5–3600) | - | No |
| `--retries <n>` | Consecutive failures that make the container unhealthy (1–10) | - | No |
| `--start-interval-secs <n>` | Seconds between checks while starting (1–60) | - | No |
| `--start-period-secs <n>` | Seconds after start during which failures do not count (0–600) | - | No |
| `--timeout-secs <n>` | Seconds one check may run (1–60, and shorter than the interval) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services apply-healthcheck`

Recreate a managed service's container to apply its healthcheck now

**Usage:** `services apply-healthcheck <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-log-ingestion`

Get this service's own container log ingestion budget

**Usage:** `services get-log-ingestion <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-log-ingestion`

Replace this service's container log ingestion budget

**Usage:** `services update-log-ingestion <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--log-ingest-kib-per-sec <n>` | KiB of log text per second per container (0–1,048,576). null = platform default | - | No |
| `--log-ingest-lines-per-sec <n>` | Lines per second per container (0–1,000,000). null = platform default | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services add-cluster-member`

Begin adding a single new member to a running cluster

**Usage:** `services add-cluster-member <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node-id <n>` | Target worker node ID | - | No |
| `--role <value>` | Member role [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services remove-cluster-member`

Remove a single member from a running cluster

**Usage:** `services remove-cluster-member <id> <member-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-cluster-member`

Get a single cluster member's current state

**Usage:** `services get-cluster-member <id> <member-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services promote-cluster-member`

Promote a replica to primary by triggering a pg_auto_failover failover

**Usage:** `services promote-cluster-member <id> <member-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services migrate-s3`

Copy every bucket of another managed S3 service into this one

**Usage:** `services migrate-s3 <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--source-service-id <n>` | Managed S3 service to copy from (RustFS or Garage) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services reveal-parameter`

Reveal one sensitive service parameter

**Usage:** `services reveal-parameter <id> <param-name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-preview-environment-variables-masked`

Get environment variables preview with masked sensitive values

**Usage:** `services get-preview-environment-variables-masked <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-preview-environment-variable-names`

Get environment variable names preview (safe - no sensitive values)

**Usage:** `services get-preview-environment-variable-names <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services issue-runtime-credentials`

Issue live connection credentials for a service in an environment

**Usage:** `services issue-runtime-credentials <id> <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `services update-resources`

Update a service's resource limits (memory, CPU caps)

**Usage:** `services update-resources <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cpu-shares <n>` | Relative CPU weight (default 1024) | - | No |
| `--memory-mb <n>` | Hard memory limit in MiB | - | No |
| `--memory-swap-mb <n>` | Memory + swap limit in MiB | - | No |
| `--nano-cpus <n>` | CPU quota in nano-cpus. 1_000_000_000 = 1 full CPU core | - | No |
| `--shm-size-mb <n>` | Shared memory (/dev/shm) size in MiB | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services retry-cluster`

Retry a failed cluster service initialization

**Usage:** `services retry-cluster <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--members <json>` | Cluster member specifications (same format as create) (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-runtime`

Inspect a service's container(s): status, restart count, OOM-killed flag, exit code, and the cgroup limits..

**Usage:** `services get-runtime <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-stats`

Sample current CPU/memory usage from each of a service's containers

**Usage:** `services get-stats <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-tls`

Get service tls

**Usage:** `services get-tls <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services restored-databases`

What the latest PostgreSQL restore brought back: its databases and users, which linked project reads each, and linked databases the backup does not have

**Usage:** `services restored-databases <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services set-link-database`

Change which database a linked project reads from this service, for example one a restore brought back. Its apps read it from their next deployment

**Usage:** `services set-link-database <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--database-provisioning-mode <value>` | project_environment (one database per environment), project (one for the whole project) or custom (the name in --custom-database-name) (one of: project_environment, project, custom) [required] | - | No |
| `--custom-database-name <value>` | The database every environment reads, in custom mode: lowercase letters, digits and _ | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services update-tls`

Update service tls

**Usage:** `services update-tls <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | The new mode. 'required' is rejected until it is supported (one of: off, available, required) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-managed-ca-certificate`

Get managed ca certificate

**Usage:** `services get-managed-ca-certificate <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services test-wal-archive-target`

Test wal archive target

**Usage:** `services test-wal-archive-target <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-workload-mode`

Get a database's workload mode (internal, operator-only)

**Usage:** `services get-workload-mode <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-workload-mode`

Change a database's workload mode (internal, operator-only)

**Usage:** `services update-workload-mode <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | Body field mode (one of: legacy, shadow, enforce) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-workload-pending-changes`

List what a recreate from the workload spec would change (internal, operator-only)

**Usage:** `services get-workload-pending-changes <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services recreate-from-workload-spec`

Recreate a database's container from its workload spec (internal, operator-only)

**Usage:** `services recreate-from-workload-spec <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--confirmed-fingerprint <value>` | The 'fingerprint' of the pending changes being confirmed | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services external-reset-pg-stat-statements`

Reset all statistics accumulated by 'pg_stat_statements' for a Postgres service

**Usage:** `services external-reset-pg-stat-statements <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--confirm` | Must be 'true' to acknowledge the global, irreversible reset [required] | - | No |
| `--no-confirm` | Set confirm to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services query`

Query commands

**Subcommands:**

- `data` - Query data from an entity with optional filters, pagination, and sorting
- `download-object` - Download an object (S3 only) as a streaming response
- `get-container-info` - Get information about a specific container

#### `services query data`

Query data from an entity with optional filters, pagination, and sorting

**Usage:** `services query data <service-id> <path> <entity> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--filters <json>` | JSON filters (backend-specific format) (JSON) | - | No |
| `--limit <n>` | Maximum number of rows to return | - | No |
| `--offset <n>` | Number of rows to skip | - | No |
| `--sort-by <value>` | Sort by field name | - | No |
| `--sort-order <value>` | Sort order (asc/desc) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `services query download-object`

Download an object (S3 only) as a streaming response

**Usage:** `services query download-object <service-id> <path> <entity> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `services query get-container-info`

Get information about a specific container

**Usage:** `services query get-container-info <service-id> <path> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services pg-upgrade`

Pg upgrade commands

**Subcommands:**

- `list` - List recent upgrades for a single service (newest first, page size 50)
- `start` - Start a new PostgreSQL major-version upgrade for a service
- `get` - Get a single upgrade by id, scoped to a service
- `cancel` - Cancel an in-flight upgrade
- `get-logs` - Get the accumulated JSONL log content for an upgrade (for dashboard display)
- `retry` - Retry a failed upgrade
- `rollback` - Roll a completed upgrade back to its pre-upgrade PGDATA volume and old image

#### `services pg-upgrade list`

List recent upgrades for a single service (newest first, page size 50)

**Usage:** `services pg-upgrade list <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade start`

Start a new PostgreSQL major-version upgrade for a service

**Usage:** `services pg-upgrade start <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from-image <value>` | Body field from_image [required] | - | No |
| `--from-version <value>` | Body field from_version [required] | - | No |
| `--to-image <value>` | Body field to_image [required] | - | No |
| `--to-version <value>` | Body field to_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade get`

Get a single upgrade by id, scoped to a service

**Usage:** `services pg-upgrade get <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade cancel`

Cancel an in-flight upgrade

**Usage:** `services pg-upgrade cancel <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade get-logs`

Get the accumulated JSONL log content for an upgrade (for dashboard display)

**Usage:** `services pg-upgrade get-logs <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade retry`

Retry a failed upgrade

**Usage:** `services pg-upgrade retry <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade rollback`

Roll a completed upgrade back to its pre-upgrade PGDATA volume and old image

**Usage:** `services pg-upgrade rollback <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

## `settings`

Manage platform settings

**Subcommands:**

- `show` (`get`) - Show current platform settings
- `update` (`set`) - Update platform settings
- `geo-status` - Show the freshness of the geolocation (GeoLite2) database
- `set-external-url` - Set the external URL for the platform
- `set-preview-domain` - Set the preview domain pattern
- `fields` - List every instance setting, or print the value at a field path (e.g. shield, otel_ingest.max_spans_per_second)
- `apply` - Change any instance setting by field path: field=value (JSON or text), field:=text, or a bare field with --value-stdin
- `admin-gate` - Show, or change with flags, which hosts and source IPs may reach the console and API listener
- `telemetry` - Show anonymous telemetry status, or turn it on/off with --enable/--disable
- `ci-builder` - Configure the external CI builder that runs builds off the control plane
- `disk-status` - Show the control plane's disk usage and alert threshold
- `public-ingress` - Show where third parties (webhooks, email tracking) call back into Temps, and whether it is reachable
- `refresh-routes` - Rebuild the proxy route table now instead of waiting for the next change
- `weekly-digest` - Generate and send the weekly digest now
- `oidc` - Oidc commands

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

### `settings geo-status`

Show the freshness of the geolocation (GeoLite2) database

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

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

### `settings fields`

List every instance setting, or print the value at a field path (e.g. shield, otel_ingest.max_spans_per_second)

**Usage:** `settings fields [path] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings apply`

Change any instance setting by field path: field=value (JSON or text), field:=text, or a bare field with --value-stdin

**Usage:** `settings apply [assignments...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from-file <path>` | Merge a JSON object of settings from a file (- for stdin) | - | No |
| `--value-stdin` | Read the value of the one bare field from stdin (for secrets) | - | No |
| `-y, --yes` | Do not ask before changing a setting that can lock you out | - | No |
| `--json` | Print the saved settings response as JSON | - | No |

### `settings admin-gate`

Show, or change with flags, which hosts and source IPs may reach the console and API listener

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--allowed-host <host>` | Host header to allow (repeatable; replaces the list) | - | No |
| `--allowed-ip <ip-or-cidr>` | Source IP or CIDR to allow (repeatable; replaces the list) | - | No |
| `--clear-hosts` | Allow any Host header | - | No |
| `--clear-ips` | Allow any source address | - | No |
| `--trust-forwarded-for` | Trust X-Forwarded-For from loopback peers | - | No |
| `--no-trust-forwarded-for` | Ignore X-Forwarded-For | - | No |
| `-y, --yes` | Do not ask before saving a gate that may lock you out | - | No |
| `--json` | Output in JSON format | - | No |

### `settings telemetry`

Show anonymous telemetry status, or turn it on/off with --enable/--disable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enable` | Send anonymous telemetry | - | No |
| `--disable` | Stop sending anonymous telemetry | - | No |
| `--json` | Output in JSON format | - | No |

### `settings ci-builder`

Configure the external CI builder that runs builds off the control plane

**Subcommands:**

- `show` - Show whether the CI builder is configured, and its settings (credentials are never returned)
- `set` - Save CI builder settings from --from-file and/or field=value assignments (e.g. limits.concurrency=2)
- `provision` - Create the builder repository and its pinned runner shim at the configured provider

#### `settings ci-builder show`

Show whether the CI builder is configured, and its settings (credentials are never returned)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings ci-builder set`

Save CI builder settings from --from-file and/or field=value assignments (e.g. limits.concurrency=2)

**Usage:** `settings ci-builder set [assignments...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from-file <path>` | JSON object of CI builder settings (- for stdin) | - | No |
| `--dispatch-token-stdin` | Read the dispatch token from stdin | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings ci-builder provision`

Create the builder repository and its pinned runner shim at the configured provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |

### `settings disk-status`

Show the control plane's disk usage and alert threshold

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings public-ingress`

Show where third parties (webhooks, email tracking) call back into Temps, and whether it is reachable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings refresh-routes`

Rebuild the proxy route table now instead of waiting for the next change

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings weekly-digest`

Generate and send the weekly digest now

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings oidc`

Oidc commands

**Subcommands:**

- `list-providers` - List oidc providers
- `create-provider` - Create oidc provider
- `get-provider` - Show one oidc provider
- `delete-provider` - Delete oidc provider
- `update-provider` - Update oidc provider
- `list-role-mappings` - List oidc role mappings
- `create-role-mapping` - Create oidc role mapping
- `test-provider` - Test oidc provider
- `list-provider-users` - List oidc provider users
- `delete-role-mapping` - Delete oidc role mapping

#### `settings oidc list-providers`

List oidc providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings oidc create-provider`

Create oidc provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--client-id <value>` | Body field client_id [required] | - | No |
| `--client-secret-stdin` | A credential [required] (read from stdin) | - | No |
| `--client-secret-file <path>` | A credential [required] (read from a file) | - | No |
| `--default-role <value>` | Body field default_role | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--group-claim <value>` | Body field group_claim | - | No |
| `--issuer-url <value>` | Body field issuer_url [required] | - | No |
| `--jit-provisioning` | Body field jit_provisioning | - | No |
| `--no-jit-provisioning` | Set jit_provisioning to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--role-claim <value>` | Body field role_claim | - | No |
| `--scopes <value>` | Body field scopes | - | No |
| `--template <value>` | Body field template | - | No |
| `--trust-idp-email` | Defaults false | - | No |
| `--no-trust-idp-email` | Set trust_idp_email to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc get-provider`

Show one oidc provider

**Usage:** `settings oidc get-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings oidc delete-provider`

Delete oidc provider

**Usage:** `settings oidc delete-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc update-provider`

Update oidc provider

**Usage:** `settings oidc update-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--client-id <value>` | Body field client_id | - | No |
| `--client-secret-stdin` | A credential (read from stdin) | - | No |
| `--client-secret-file <path>` | A credential (read from a file) | - | No |
| `--default-role <value>` | Body field default_role | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--group-claim <value>` | Body field group_claim | - | No |
| `--issuer-url <value>` | Body field issuer_url | - | No |
| `--jit-provisioning` | Body field jit_provisioning | - | No |
| `--no-jit-provisioning` | Set jit_provisioning to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--role-claim <value>` | Body field role_claim | - | No |
| `--scopes <value>` | Body field scopes | - | No |
| `--template <value>` | Body field template | - | No |
| `--trust-idp-email` | Body field trust_idp_email | - | No |
| `--no-trust-idp-email` | Set trust_idp_email to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc list-role-mappings`

List oidc role mappings

**Usage:** `settings oidc list-role-mappings <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings oidc create-role-mapping`

Create oidc role mapping

**Usage:** `settings oidc create-role-mapping <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--idp-group <value>` | Body field idp_group [required] | - | No |
| `--priority <n>` | Body field priority [required] | - | No |
| `--role <value>` | Body field role [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc test-provider`

Test oidc provider

**Usage:** `settings oidc test-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `settings oidc list-provider-users`

List oidc provider users

**Usage:** `settings oidc list-provider-users <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `settings oidc delete-role-mapping`

Delete oidc role mapping

**Usage:** `settings oidc delete-role-mapping <mapping-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

## `users`

Manage platform users

**Subcommands:**

- `get` - Show one platform user
- `list` (`ls`) - List all users
- `create` (`add`) - Create a new user
- `me` - Show current user info
- `remove` (`rm`) - Remove a user
- `restore` - Restore a deleted user
- `reset-password` - Reset another user's password to a generated temporary one. The user is signed out of every browser session and must choose a new password at next sign-in
- `reset-mfa` - Remove another user's MFA (authenticator and recovery codes) after they lost both. The user is signed out of every session. Password sign-in is blocked when require_mfa_for_admins applies; arrange SSO access or a temporary policy change before resetting MFA. Otherwise, sign in with a password to enroll again
- `role` - Manage user roles
- `update-self` - Update current user's information
- `disable-mfa` - Disable mfa
- `setup-mfa` - Setup mfa
- `verify-and-enable-mfa` - Verify and enable mfa
- `change-password-self` - Change password self
- `update` - Update user information (admin only)

### `users get`

Show one platform user

**Usage:** `users get <user-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

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
| `--must-change-password` | Make the user choose a new password at first sign-in | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--password-file <path>` | Read the password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the password from stdin instead of --password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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

### `users reset-password`

Reset another user's password to a generated temporary one. The user is signed out of every browser session and must choose a new password at next sign-in

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompt (for automation) | - | No |

### `users reset-mfa`

Remove another user's MFA (authenticator and recovery codes) after they lost both. The user is signed out of every session. Password sign-in is blocked when require_mfa_for_admins applies; arrange SSO access or a temporary policy change before resetting MFA. Otherwise, sign in with a password to enroll again

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompt (for automation) | - | No |

### `users role`

Manage user roles

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `--add <role>` | Add a role to user | - | No |
| `--remove <role>` | Remove a role from user | - | No |

### `users update-self`

Update current user's information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--email <value>` | Body field email | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `users disable-mfa`

Disable mfa

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--code <value>` | Body field code [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `users setup-mfa`

Setup mfa

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--current-password-stdin` | Required to enroll MFA on an account that has a password set (read from stdin) | - | No |
| `--current-password-file <path>` | Required to enroll MFA on an account that has a password set (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `users verify-and-enable-mfa`

Verify and enable mfa

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--code <value>` | Body field code [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `users change-password-self`

Change password self

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--current-password-stdin` | Body field current_password [required] (read from stdin) | - | No |
| `--current-password-file <path>` | Body field current_password [required] (read from a file) | - | No |
| `--mfa-code <value>` | TOTP code (or recovery code) | - | No |
| `--new-password-stdin` | Body field new_password [required] (read from stdin) | - | No |
| `--new-password-file <path>` | Body field new_password [required] (read from a file) | - | No |
| `--revoke-other-sessions` | When true, every session OTHER than the one making this request is revoked | - | No |
| `--no-revoke-other-sessions` | Set revoke_other_sessions to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `users update`

Update user information (admin only)

**Usage:** `users update <user-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--email <value>` | Body field email | - | No |
| `--name <value>` | Deprecated: the old name of 'username', still accepted | - | No |
| `--username <value>` | The new username, as in 'CreateUserRequest' | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

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
| `--page <n>` | Page of 100 teams (default: 1) | - | No |
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

**Usage:** `teams show <team> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `teams update`

Update a team name or description

**Usage:** `teams update <team> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New team name | - | No |
| `-d, --description <description>` | New description | - | No |
| `--json` | Output in JSON format | - | No |

### `teams delete` (alias: `rm`)

Delete a team (removes its members and project grants)

**Usage:** `teams delete <team> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `teams members`

Manage team membership

**Subcommands:**

- `get` - Show one member of a team
- `list` (`ls`) - List a team's members
- `add` - Add a user to a team
- `set-role` - Change a member's role in the team
- `remove` (`rm`) - Remove a user from a team

#### `teams members get`

Show one member of a team

**Usage:** `teams members get <team-id> <user-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `teams members list` (alias: `ls`)

List a team's members

**Usage:** `teams members list <team> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `teams members add`

Add a user to a team

**Usage:** `teams members add <team> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --user <user>` | User id or email | - | No |
| `-r, --role <role>` | Team role (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

#### `teams members set-role`

Change a member's role in the team

**Usage:** `teams members set-role <team> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --user <user>` | User id or email | - | No |
| `-r, --role <role>` | Team role (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

#### `teams members remove` (alias: `rm`)

Remove a user from a team

**Usage:** `teams members remove <team> [options]`

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

**Usage:** `access grant <team> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-r, --role <role>` | Role the team holds on the project (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

### `access revoke`

Revoke a team's access to a project

**Usage:** `access revoke <team> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-y, --yes` | Skip confirmation | - | No |

## `project-groups` (alias: `pg`)

Manage Projects (what the console calls a Project): groups of services that ship together and share a repository, variables and path routes. A service is what the `projects` command manages; the `services` command manages databases and caches, not these.

**Subcommands:**

- `list` (`ls`) - List projects (groups) and their services
- `show` (`get`) - Show a project (group), by slug or id
- `create` (`new`) - Create a project (group), optionally with services
- `update` (`edit`) - Rename a project (group) or change its description
- `delete` (`rm`) - Delete a project (group); its services keep running as standalone services
- `add` - Add services to a project (group), by slug or id as listed by `projects list`; moves them out of any other project
- `remove` - Remove a service (slug or id) from a project (group); it becomes a standalone service
- `repo` - The repository a project deploys its services from (monorepos)
- `import` - Scan the project's repository and add its apps as services that follow it
- `follow` - Make a service follow the project's repository and branch (keeps its directory)
- `unfollow` - Stop a service following the project's repository; it keeps its current settings
- `routes` - Serve a path of one service's domains from another service (e.g. /api → backend)
- `vars` - Variables shared by every service in a project (group)

### `project-groups list` (alias: `ls`)

List projects (groups) and their services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default 1) | - | No |
| `--per-page <n>` | How many per page (default 100, at most 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `project-groups show` (alias: `get`)

Show a project (group), by slug or id

**Usage:** `project-groups show <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `project-groups create` (alias: `new`)

Create a project (group), optionally with services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Project name | - | No |
| `-s, --slug <slug>` | URL slug (derived from the name when omitted) | - | No |
| `-d, --description <text>` | What the services do together | - | No |
| `--service <service...>` | Services to put in the project, by slug or id as listed by `projects list` (apps, not the databases of the `services` command) | - | No |
| `--json` | Output in JSON format | - | No |

### `project-groups update` (alias: `edit`)

Rename a project (group) or change its description

**Usage:** `project-groups update <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New name | - | No |
| `-s, --slug <slug>` | New slug | - | No |
| `-d, --description <text>` | New description ("" clears it) | - | No |
| `--json` | Output in JSON format | - | No |

### `project-groups delete` (alias: `rm`)

Delete a project (group); its services keep running as standalone services

**Usage:** `project-groups delete <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `project-groups add`

Add services to a project (group), by slug or id as listed by `projects list`; moves them out of any other project

**Usage:** `project-groups add <group> <services...>`

### `project-groups remove`

Remove a service (slug or id) from a project (group); it becomes a standalone service

**Usage:** `project-groups remove <group> <service>`

### `project-groups repo`

The repository a project deploys its services from (monorepos)

**Subcommands:**

- `show` - Show the project's repository and which services follow it
- `set` - Point the project at a repository and branch; services that follow it are updated to match
- `unset` - Remove the repository; services keep their current settings and stop following

#### `project-groups repo show`

Show the project's repository and which services follow it

**Usage:** `project-groups repo show <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups repo set`

Point the project at a repository and branch; services that follow it are updated to match

**Usage:** `project-groups repo set <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-c, --connection <id>` | Git connection id (see: git-providers / connections) | - | No |
| `--public <provider>` | Use a public repository without a connection: github or gitlab | - | No |
| `-r, --repo <owner/name\|url>` | Repository, e.g. acme/shop; a github.com/gitlab.com URL implies --public | - | Yes |
| `-b, --branch <branch>` | Branch to deploy from, e.g. main | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `project-groups repo unset`

Remove the repository; services keep their current settings and stop following

**Usage:** `project-groups repo unset <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `project-groups import`

Scan the project's repository and add its apps as services that follow it

**Usage:** `project-groups import <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --path <dir...>` | Only these app directories (default: every app without a service) | - | No |
| `--dry-run` | List what would be created without creating anything | - | No |
| `--timeout <minutes>` | Stop waiting after this long (default 30); the import keeps running on the server | - | No |
| `--fresh` | Scan a public repository again instead of using the cached scan | - | No |
| `--base-url <url>` | HTTPS origin of the self-hosted GitLab a public repository lives on | - | No |
| `--json` | Output in JSON format | - | No |

### `project-groups follow`

Make a service follow the project's repository and branch (keeps its directory)

**Usage:** `project-groups follow <group> <service>`

### `project-groups unfollow`

Stop a service following the project's repository; it keeps its current settings

**Usage:** `project-groups unfollow <group> <service>`

### `project-groups routes`

Serve a path of one service's domains from another service (e.g. /api → backend)

**Subcommands:**

- `list` (`ls`) - List path routes and how each applies per environment
- `add` - Send requests under <prefix> on <source>'s domains to <target>, e.g. `routes add shop web /api api`
- `remove` (`rm`) - Stop routing <prefix>; <source> serves it itself again

#### `project-groups routes list` (alias: `ls`)

List path routes and how each applies per environment

**Usage:** `project-groups routes list <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups routes add`

Send requests under <prefix> on <source>'s domains to <target>, e.g. `routes add shop web /api api`

**Usage:** `project-groups routes add <group> <source> <prefix> <target> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--strip-prefix` | Forward /api/users as /users (default: keep the full path) | - | No |
| `--json` | Output in JSON format | - | No |

#### `project-groups routes remove` (alias: `rm`)

Stop routing <prefix>; <source> serves it itself again

**Usage:** `project-groups routes remove <group> <source> <prefix> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `project-groups vars`

Variables shared by every service in a project (group)

**Subcommands:**

- `get` - Show one shared variable (a secret value is never shown)
- `list` (`ls`) - List shared variables (secret values are never shown)
- `set` - Create or update a shared variable; a service's own variable still wins. Omit the value to type it at a hidden prompt, or read it with --value-file or --value-stdin (keeps secrets out of shell history)
- `inherited` - Show the shared variables a service (slug or id) receives from its project, and which of them the service's own variables override
- `unset` - Remove a shared variable

#### `project-groups vars get`

Show one shared variable (a secret value is never shown)

**Usage:** `project-groups vars get <group-id> <var-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups vars list` (alias: `ls`)

List shared variables (secret values are never shown)

**Usage:** `project-groups vars list <group> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups vars set`

Create or update a shared variable; a service's own variable still wins. Omit the value to type it at a hidden prompt, or read it with --value-file or --value-stdin (keeps secrets out of shell history)

**Usage:** `project-groups vars set <group> <key> [value] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <slug>` | Only this environment (e.g. production); default: all | - | No |
| `--secret` | Write-only: the value can never be read back | - | No |
| `--value-file <path>` | Read the value from a file (- reads stdin), minus one final newline | - | No |
| `--value-stdin` | Read the value from stdin, minus one final newline, e.g. `cat key.pem \| ... vars set shop TLS_KEY --secret --value-stdin` | - | No |
| `--preview` | Also send an all-environments variable to preview deployments (off by default) | - | No |
| `--no-preview` | Do not send it to preview deployments | - | No |
| `--json` | Output in JSON format | - | No |

#### `project-groups vars inherited`

Show the shared variables a service (slug or id) receives from its project, and which of them the service's own variables override

**Usage:** `project-groups vars inherited <service> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `project-groups vars unset`

Remove a shared variable

**Usage:** `project-groups vars unset <group> <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <slug>` | The environment it was scoped to; default: all | - | No |
| `-y, --yes` | Skip confirmation | - | No |

## `instantiations` (alias: `inst`)

Follow and retry template creations and imports

**Subcommands:**

- `list` (`ls`) - List your recent template creations and imports, newest first
- `show` (`get`) - Show every step of a template creation or import
- `retry` - Continue from the step that failed; finished steps are not repeated

### `instantiations list` (alias: `ls`)

List your recent template creations and imports, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--group <project>` | Only those that added services to this project (group slug or id) | - | No |
| `--page <n>` | Page number (default 1) | - | No |
| `--per-page <n>` | How many per page (default 20, at most 100) | - | No |
| `--limit <n>` | Same as --per-page | - | No |
| `--json` | Output in JSON format | - | No |

### `instantiations show` (alias: `get`)

Show every step of a template creation or import

**Usage:** `instantiations show <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--wait` | Wait until it finishes | - | No |
| `--timeout <minutes>` | With --wait: stop waiting after this long (default 30) | - | No |
| `--json` | Output in JSON format | - | No |

### `instantiations retry`

Continue from the step that failed; finished steps are not repeated

**Usage:** `instantiations retry <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--no-wait` | Return right away instead of waiting until it finishes | - | No |
| `--timeout <minutes>` | Stop waiting after this long (default 30); it keeps running on the server | - | No |
| `--json` | Output in JSON format | - | No |

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
- `get-ai-harness-status` - Whether the caller's external AI harness has connected
- `update-api-key` - Update api key
- `rotate-api-key` - Rotate api key

### `apikeys list` (alias: `ls`)

List all API keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Keys per page (max 100) | - | No |

### `apikeys create` (alias: `add`)

Create a new API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | API key name | - | No |
| `-r, --role <role>` | Role type (admin, platform_admin, user, reader, api_reader, custom, metrics_ingest) | - | No |
| `-e, --expires-in <days>` | Expires in N days (7, 30, 90, 365) | - | No |
| `-p, --permissions <permissions>` | Comma-separated list of permissions | - | No |
| `--project-id <id>` | Confine to one project’s deployment and environment APIs | - | No |
| `--environment-id <id>` | Confine to one environment (requires --project-id) | - | No |
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

### `apikeys get-ai-harness-status`

Whether the caller's external AI harness has connected

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `apikeys update-api-key`

Update api key

**Usage:** `apikeys update-api-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expires-at <value>` | Body field expires_at | - | No |
| `--is-active` | Body field is_active | - | No |
| `--no-is-active` | Set is_active to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--permissions <value>` | Body field permissions (repeatable) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `apikeys rotate-api-key`

Rotate api key

**Usage:** `apikeys rotate-api-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

## `monitors` (alias: `monitoring`)

Manage uptime monitors for status pages

**Subcommands:**

- `list` (`ls`) - List all monitors for a project
- `create` (`add`) - Create a new monitor for a project
- `add-target` - Monitor a URL, port, TLS certificate or DNS record outside Temps
- `update-target` - Change a monitor of a target outside Temps; omitted flags keep their value
- `locations` - List where monitors checked from worker nodes are checked from
- `show` - Show monitor details and current status
- `remove` (`rm`) - Delete a monitor
- `status` - Get current status — all monitors for a project, or a single monitor by ID
- `history` - Get monitor uptime history
- `get-health` - Get monitor-based health summaries for multiple projects in a single query
- `update` - Update a monitor's name, check interval or check path
- `get-bucketed-status` - Get bucketed status data for a monitor using TimescaleDB
- `crons` - Crons commands
- `http-checks` - Http checks commands

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

### `monitors add-target`

Monitor a URL, port, TLS certificate or DNS record outside Temps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--kind <kind>` | http, tcp, tls or dns | - | Yes |
| `--target <target>` | URL (http), host:port (tcp, tls) or host name (dns) | - | Yes |
| `-n, --name <name>` | Monitor name | - | Yes |
| `-i, --interval <seconds>` | Check interval in seconds (default 60, at least 30) | - | No |
| `--keyword-absent` | Fail when --keyword is present instead of missing | - | No |
| `--from-nodes` | Also check from online worker nodes; down only when most locations agree | - | No |
| `--method <method>` | HTTP method (default GET) | - | No |
| `--expect-status <codes>` | Codes or classes counted as up, e.g. 200,204 or 2xx (default 2xx or 3xx) | - | No |
| `--keyword <text>` | Text the response body must contain (first 256 KiB; never stored) | - | No |
| `--degraded-after-ms <ms>` | Slower than this is degraded (http, tcp) | - | No |
| `--warn-days <days>` | Degraded when the certificate expires within this many days (tls, default 14) | - | No |
| `--record <type>` | DNS record type: A, AAAA, CNAME, MX, TXT, NS (default A) | - | No |
| `--expect <values>` | Comma-separated DNS values; any match is up (default: any answer) | - | No |
| `--header-file <path>` | File of "Name: value" request headers; stored encrypted, never shown | - | No |
| `--body-file <path>` | File with the request body; stored encrypted | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors update-target`

Change a monitor of a target outside Temps; omitted flags keep their value

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--monitor-id <id>` | Monitor ID | - | Yes |
| `--kind <kind>` | Change the check kind (pass the check flags for it) | - | No |
| `--target <target>` | New target | - | No |
| `-n, --name <name>` | New name | - | No |
| `-i, --interval <seconds>` | Check interval in seconds | - | No |
| `--keyword-absent` | Fail when --keyword is present instead of missing | - | No |
| `--pause` | Stop checking | - | No |
| `--resume` | Start checking again | - | No |
| `--from-nodes` | Also check from online worker nodes | - | No |
| `--no-from-nodes` | Check from this server only | - | No |
| `--method <method>` | HTTP method (default GET) | - | No |
| `--expect-status <codes>` | Codes or classes counted as up, e.g. 200,204 or 2xx (default 2xx or 3xx) | - | No |
| `--keyword <text>` | Text the response body must contain (first 256 KiB; never stored) | - | No |
| `--degraded-after-ms <ms>` | Slower than this is degraded (http, tcp) | - | No |
| `--warn-days <days>` | Degraded when the certificate expires within this many days (tls, default 14) | - | No |
| `--record <type>` | DNS record type: A, AAAA, CNAME, MX, TXT, NS (default A) | - | No |
| `--expect <values>` | Comma-separated DNS values; any match is up (default: any answer) | - | No |
| `--header-file <path>` | File of "Name: value" request headers; stored encrypted, never shown | - | No |
| `--body-file <path>` | File with the request body; stored encrypted | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors locations`

List where monitors checked from worker nodes are checked from

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

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

### `monitors get-health`

Get monitor-based health summaries for multiple projects in a single query

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-ids <value>` | Comma-separated list of project IDs [required] | - | No |
| `--environment-id <n>` | Select one environment within the authorized projects; defaults to production | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors update`

Update a monitor's name, check interval or check path

**Usage:** `monitors update <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--check-interval-seconds <n>` | How often to probe, in seconds. **Clamped, not rejected**, exactly like on create: values below 30 are... | - | No |
| `--check-path <value>` | Path probed on the monitored environment; must start with '/' | - | No |
| `--name <value>` | Display name; 1 to 255 characters after trimming | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors get-bucketed-status`

Get bucketed status data for a monitor using TimescaleDB

**Usage:** `monitors get-bucketed-status <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--interval <value>` | Bucket interval: '5min', 'hourly', or 'daily' (default: hourly) | - | No |
| `--start-time <value>` | Start time (ISO 8601) (default: 24 hours ago) | - | No |
| `--end-time <value>` | End time (ISO 8601) (default: now) | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors crons`

Crons commands

**Subcommands:**

- `list` - List cron monitors
- `create` - Create cron monitor
- `delete` - Delete cron monitor
- `get` - Get cron monitor
- `update` - Update cron monitor
- `list-runs` - List cron monitor runs

#### `monitors crons list`

List cron monitors

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons create`

Create cron monitor

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--environment-id <n>` | Body field environment_id | - | No |
| `--slug <value>` | Body field slug [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons delete`

Delete cron monitor

**Usage:** `monitors crons delete <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons get`

Get cron monitor

**Usage:** `monitors crons get <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons update`

Update cron monitor

**Usage:** `monitors crons update <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Body field config (JSON) | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons list-runs`

List cron monitor runs

**Usage:** `monitors crons list-runs <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors http-checks`

Http checks commands

**Subcommands:**

- `detect-env-credential` - Detect env credential
- `list-variable-history` - List variable history
- `list` - List http checks
- `create` - Create http check
- `get-capabilities` - Get http checks capabilities
- `list-presets` - List http check presets
- `get` - Show one http check
- `delete` - Delete http check
- `set-enabled` - Set http check enabled
- `update` - Update http check
- `run` - Run http check
- `detect-secret-credential` - Detect secret credential
- `list-secret-history` - List secret history

#### `monitors http-checks detect-env-credential`

Detect env credential

**Usage:** `monitors http-checks detect-env-credential <env-var-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks list-variable-history`

List variable history

**Usage:** `monitors http-checks list-variable-history <env-var-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks list`

List http checks

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks create`

Create http check

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--credential-stdin` | Body field credential (read from stdin) | - | No |
| `--credential-file <path>` | Body field credential (read from a file) | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--env-var-id <n>` | Body field env_var_id | - | No |
| `--interval-seconds <n>` | Body field interval_seconds | - | No |
| `--kind <value>` | 'http' (default) calls an endpoint; 'local' reads expiring items (certificates, SSH certificates, OpenPGP... (one of: http, local) | - | No |
| `--local <json>` | Body field local (JSON) | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--secret-id <n>` | Project secret to check | - | No |
| `--spec <json>` | This is a declarative HTTP recipe, never executable code (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks get-capabilities`

Get http checks capabilities

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks list-presets`

List http check presets

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks get`

Show one http check

**Usage:** `monitors http-checks get <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks delete`

Delete http check

**Usage:** `monitors http-checks delete <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks set-enabled`

Set http check enabled

**Usage:** `monitors http-checks set-enabled <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks update`

Update http check

**Usage:** `monitors http-checks update <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--credential-stdin` | Body field credential (read from stdin) | - | No |
| `--credential-file <path>` | Body field credential (read from a file) | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--env-var-id <n>` | Body field env_var_id | - | No |
| `--interval-seconds <n>` | Body field interval_seconds | - | No |
| `--kind <value>` | 'http' (default) calls an endpoint; 'local' reads expiring items (certificates, SSH certificates, OpenPGP... (one of: http, local) | - | No |
| `--local <json>` | Body field local (JSON) | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--secret-id <n>` | Project secret to check | - | No |
| `--spec <json>` | This is a declarative HTTP recipe, never executable code (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks run`

Run http check

**Usage:** `monitors http-checks run <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks detect-secret-credential`

Detect secret credential

**Usage:** `monitors http-checks detect-secret-credential <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks list-secret-history`

List secret history

**Usage:** `monitors http-checks list-secret-history <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

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
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Webhooks per page (max 100) | - | No |
| `--sort-by <value>` | Sort column (default created_at) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

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
| `--secret-file <path>` | Read the webhook signing secret from a file (- reads stdin) instead of --secret | - | No |
| `--secret-stdin` | Read the webhook signing secret from stdin instead of --secret | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
| `--secret-file <path>` | Read the webhook signing secret from a file (- reads stdin) instead of --secret | - | No |
| `--secret-stdin` | Read the webhook signing secret from stdin instead of --secret | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
- `get-environment-variable` - Get container environment variable
- `metrics-get-history` - Fetch a time-series range for a single container resource metric (recorded by the container health monitor..
- `stream-metrics` - Stream container metrics via Server-Sent Events (SSE)

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

### `containers get-environment-variable`

Get container environment variable

**Usage:** `containers get-environment-variable <environment-id> <container-id> <variable-name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `containers metrics-get-history`

Fetch a time-series range for a single container resource metric (recorded by the container health monitor..

**Usage:** `containers metrics-get-history <environment-id> <container-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--metric <value>` | Dotted metric name, e.g. 'container.cpu_percent' or 'container.memory_used_bytes' [required] | - | No |
| `--range <value>` | Time window: '1h', '6h', '24h', or '7d' (defaults to '1h') | - | No |
| `--json` | Output in JSON format | - | No |

### `containers stream-metrics`

Stream container metrics via Server-Sent Events (SSE)

**Usage:** `containers stream-metrics <environment-id> <container-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--interval <n>` | Update interval in milliseconds (default: 1000) | - | No |
| `--json` | Output in JSON format | - | No |

## `cluster`

Cluster-wide multi-node operations

**Subcommands:**

- `dns` - Cluster DNS resolver (ADR-024) operations
- `network` - The compute network workloads get their addresses from
- `join-token` - The shared token worker nodes join with
- `enrollment-tokens` - Short-lived, single-use tokens a new node enrolls with
- `rotate-ca` - Replace a compromised cluster CA: every node re-enrolls and outstanding enrollment tokens are revoked

### `cluster dns`

Cluster DNS resolver (ADR-024) operations

**Subcommands:**

- `status` - Show whether cluster DNS is healthy across every node — resolver status, last sync, and errors — without SSHing into a node to read logs
- `resolvers` - Each node's resolver: health, version, records served and last sync
- `records` - The internal zone's records and who owns them, newest first
- `queries` - Recent queries the internal resolvers answered, newest first
- `traffic` - Queries per outcome over time for one node's resolver
- `resolve` - Resolve a name from one node's resolver, optionally as a project or sandbox would see it

#### `cluster dns status`

Show whether cluster DNS is healthy across every node — resolver status, last sync, and errors — without SSHing into a node to read logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster dns resolvers`

Each node's resolver: health, version, records served and last sync

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster dns records`

The internal zone's records and who owns them, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--search <text>` | Only records whose name contains this | - | No |
| `--node <id>` | Only records served by this node | - | No |
| `--owner-kind <kind>` | Only records owned by this kind (e.g. environment, service, sandbox) | - | No |
| `--page <n>` | Page (default 1) | - | No |
| `--page-size <n>` | Records per page (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster dns queries`

Recent queries the internal resolvers answered, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Only this node | - | No |
| `--outcome <outcome>` | Only this outcome (e.g. answered, nxdomain, refused) | - | No |
| `--zone <zone>` | Only this zone | - | No |
| `--name <name>` | Only this queried name | - | No |
| `--project <id>` | Only queries from this project ID | - | No |
| `--page <n>` | Page (default 1) | - | No |
| `--page-size <n>` | Queries per page (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster dns traffic`

Queries per outcome over time for one node's resolver

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node (default: the control plane) | - | No |
| `--range <range>` | Time range, e.g. 1h, 24h, 7d | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster dns resolve`

Resolve a name from one node's resolver, optionally as a project or sandbox would see it

**Usage:** `cluster dns resolve <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node whose resolver answers | - | Yes |
| `--type <type>` | Record type (A, AAAA, CNAME, SRV, TXT) | - | No |
| `--project <id>` | Ask as this project ID | - | No |
| `--sandbox <id>` | Ask as this sandbox | - | No |
| `--json` | Output in JSON format | - | No |

### `cluster network`

The compute network workloads get their addresses from

**Subcommands:**

- `show` - Show the compute pool, whether it can change, and each node's allocation
- `set` - Change the compute pool (only while no allocation uses it)
- `new-network-id` - Give this cluster its own VXLAN network ID, so it cannot collide with another cluster on the same network

#### `cluster network show`

Show the compute pool, whether it can change, and each node's allocation

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster network set`

Change the compute pool (only while no allocation uses it)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--compute-pool <cidr>` | CIDR workloads get addresses from | - | Yes |
| `--overlay-network <name>` | Docker overlay network name | - | No |
| `--subnet-prefix <len>` | Per-node subnet prefix length | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster network new-network-id`

Give this cluster its own VXLAN network ID, so it cannot collide with another cluster on the same network

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `cluster join-token`

The shared token worker nodes join with

**Subcommands:**

- `status` - Show whether a join token is configured
- `generate` - Generate a new join token (replaces the current one; shown once)
- `revoke` - Revoke the join token; no new node can join with it

#### `cluster join-token status`

Show whether a join token is configured

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster join-token generate`

Generate a new join token (replaces the current one; shown once)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask before replacing an existing token | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster join-token revoke`

Revoke the join token; no new node can join with it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `cluster enrollment-tokens`

Short-lived, single-use tokens a new node enrolls with

**Subcommands:**

- `list` (`ls`) - List valid enrollment tokens (the tokens themselves are never returned)
- `create` (`mint`) - Mint an enrollment token (shown once)
- `revoke` - Revoke an enrollment token

#### `cluster enrollment-tokens list` (alias: `ls`)

List valid enrollment tokens (the tokens themselves are never returned)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster enrollment-tokens create` (alias: `mint`)

Mint an enrollment token (shown once)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ttl <seconds>` | How long it is valid | - | No |
| `--max-uses <n>` | How many nodes may enroll with it | - | No |
| `--node <name>` | Only a node with this name may use it | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster enrollment-tokens revoke`

Revoke an enrollment token

**Usage:** `cluster enrollment-tokens revoke <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `cluster rotate-ca`

Replace a compromised cluster CA: every node re-enrolls and outstanding enrollment tokens are revoked

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-fingerprint <sha256>` | Fingerprint of the current CA, read through a trusted channel just before | - | Yes |
| `-y, --yes` | Do not ask; sends the confirmation "ROTATE CLUSTER CA" for you | - | No |
| `--json` | Output in JSON format | - | No |

## `nodes`

Worker nodes and workload placement

**Subcommands:**

- `capability` - Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued
- `mesh` - WireGuard mesh: lets nodes that only share the internet with the control plane join it, encrypted. Shows whether it is on and how each node is connected
- `pair` - Pair nodes this control plane dials: for a control plane nodes cannot reach (a laptop, a server behind NAT). Lists recent pairings and their progress
- `ssh` - Add servers over SSH: the control plane logs in, installs temps if needed, pairs the server and starts its agent. Lists recent ones and their progress
- `resources` - What the scheduler keeps for the OS and Temps on a node, and whether a placement that does not fit the remaining capacity is refused
- `state-status` - Show the workload plane's desired-state stream: whether it runs here, its lag and counters
- `show` - Show one node: status, addresses, labels and capacity
- `containers` - List the containers running on a node
- `drain` - Stop scheduling on a node and move its replicas elsewhere (redeploys the affected environments)
- `undrain` - Let a drained node accept new deployments again
- `remove` (`rm`) - Remove a drained node from the cluster and from every placement that targets it
- `build-limits` - Show a worker node's build limits, or change them with flags
- `public-ingress` - Show or change whether a node serves public HTTP(S) traffic itself, and on which ports and addresses
- `workload-mode` - Show an environment's workload mode, or set it (legacy, shadow, enforce); operator-only

### `nodes capability`

Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes mesh`

WireGuard mesh: lets nodes that only share the internet with the control plane join it, encrypted. Shows whether it is on and how each node is connected

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `enable` - Turn the WireGuard mesh on. The control plane and every agent move onto it within a minute; it can be turned off again only while no node is on it
- `disable` - Turn the WireGuard mesh off. Refused, with the reason, while a node is on it, a pairing is in progress or the cluster network runs over it. The control plane stops tending its end; the interface itself goes away when `temps serve` restarts
- `hub` - Mesh hub: a member that relays between members that cannot reach each other (two nodes behind NAT). Shows the hub and every pair it carries
- `doctor` - Check the mesh from the control plane: its end, every node link and every pairing in progress, each failure with what fixes it. Exits 1 when a check fails. For a node's own end, run `temps doctor mesh` on it

#### `nodes mesh enable`

Turn the WireGuard mesh on. The control plane and every agent move onto it within a minute; it can be turned off again only while no node is on it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cidr <cidr>` | Mesh address pool (private IPv4, clear of the compute pool) | - | No |
| `--port <port>` | UDP port every node must accept from the others | - | No |
| `--node-api-port <port>` | TCP port nodes reach this control plane on over the mesh (default: the mesh port) | - | No |
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

#### `nodes mesh disable`

Turn the WireGuard mesh off. Refused, with the reason, while a node is on it, a pairing is in progress or the cluster network runs over it. The control plane stops tending its end; the interface itself goes away when `temps serve` restarts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

#### `nodes mesh hub`

Mesh hub: a member that relays between members that cannot reach each other (two nodes behind NAT). Shows the hub and every pair it carries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `set` - Make <member> the hub: control-plane, or a node name. Pairs that never connect move onto it within a few minutes. The hub can read the traffic it relays: pick your own machine
- `unset` - Remove the hub: relayed pairs go back to trying the direct path

##### `nodes mesh hub set`

Make <member> the hub: control-plane, or a node name. Pairs that never connect move onto it within a few minutes. The hub can read the traffic it relays: pick your own machine

**Usage:** `nodes mesh hub set <member> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

##### `nodes mesh hub unset`

Remove the hub: relayed pairs go back to trying the direct path

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

#### `nodes mesh doctor`

Check the mesh from the control plane: its end, every node link and every pairing in progress, each failure with what fixes it. Exits 1 when a check fails. For a node's own end, run `temps doctor mesh` on it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes pair`

Pair nodes this control plane dials: for a control plane nodes cannot reach (a laptop, a server behind NAT). Lists recent pairings and their progress

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `create` - Start pairing the node at --address. Prints the one command to run on it; the control plane then dials it on the mesh port until it answers (30 minutes)
- `cancel` - Cancel a pending pairing: its command stops working and its address is released

#### `nodes pair create`

Start pairing the node at --address. Prints the one command to run on it; the control plane then dials it on the mesh port until it answers (30 minutes)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--address <ip[:port]>` | The node's public IP (and mesh port, if not the default) | - | Yes |
| `--name <name>` | Name the node registers under (default: worker-<random>) | - | No |
| `--json` | Output in JSON format (includes the command, which holds a secret) | - | No |

#### `nodes pair cancel`

Cancel a pending pairing: its command stops working and its address is released

**Usage:** `nodes pair cancel <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes ssh`

Add servers over SSH: the control plane logs in, installs temps if needed, pairs the server and starts its agent. Lists recent ones and their progress

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page to show (default 1) | - | No |
| `--per-page <n>` | Servers per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `add` - Add the server at --host. Shows its SSH host key to confirm first (or pass --host-key). Credentials are used for this enrollment only and never stored
- `host-key` - Read the SSH host key of the server at --host, to verify it before `nodes ssh add --host-key`. Nothing is logged in to or changed
- `show` - Show one enrollment: its progress and the log with the server output

#### `nodes ssh add`

Add the server at --host. Shows its SSH host key to confirm first (or pass --host-key). Credentials are used for this enrollment only and never stored

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--host <host>` | Hostname or IP address of the server | - | Yes |
| `--port <port>` | SSH port | `22` | No |
| `--user <user>` | User to log in as: root, or a user with sudo | `root` | No |
| `--identity-file <path>` | Log in with this private key | - | No |
| `--ask-passphrase` | Prompt for the private key passphrase | - | No |
| `--passphrase-stdin` | Read the private key passphrase from stdin (not with --password-stdin) | - | No |
| `--password-stdin` | Read the password from stdin (default: prompt for it) | - | No |
| `--host-key <fingerprint>` | The SHA256:… host key fingerprint you verified (see `nodes ssh host-key`) | - | No |
| `--name <name>` | Name the node registers under (default: worker-<random>) | - | No |
| `--node-address <ip[:port]>` | The server's public address for WireGuard, if not the one SSH connects to | - | No |
| `--no-wait` | Return once started instead of following the progress | - | No |
| `--json` | Output in JSON format | - | No |

#### `nodes ssh host-key`

Read the SSH host key of the server at --host, to verify it before `nodes ssh add --host-key`. Nothing is logged in to or changed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--host <host>` | Hostname or IP address of the server | - | Yes |
| `--port <port>` | SSH port | `22` | No |
| `--json` | Output in JSON format | - | No |

#### `nodes ssh show`

Show one enrollment: its progress and the log with the server output

**Usage:** `nodes ssh show <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes resources`

What the scheduler keeps for the OS and Temps on a node, and whether a placement that does not fit the remaining capacity is refused

**Subcommands:**

- `show` - Show a node's reservation and resource policy (node id; 0 or "control-plane" for the control plane)
- `set` - Change a node's resource policy or reservation. Giving only one of --reserve-cpu and --reserve-memory keeps the node's current value for the other

#### `nodes resources show`

Show a node's reservation and resource policy (node id; 0 or "control-plane" for the control plane)

**Usage:** `nodes resources show <node> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `nodes resources set`

Change a node's resource policy or reservation. Giving only one of --reserve-cpu and --reserve-memory keeps the node's current value for the other

**Usage:** `nodes resources set <node> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--policy <policy>` | best_effort (reservations not checked) or enforced (placements that do not fit are refused) | - | No |
| `--reserve-cpu <millis>` | CPU kept for the OS and Temps, in millicores (1000 = one core) | - | No |
| `--reserve-memory <mb>` | Memory kept for the OS and Temps, in MB | - | No |
| `--reset-reserved` | Go back to the default reservation for the node's role | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes state-status`

Show the workload plane's desired-state stream: whether it runs here, its lag and counters

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes show`

Show one node: status, addresses, labels and capacity

**Usage:** `nodes show <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes containers`

List the containers running on a node

**Usage:** `nodes containers <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes drain`

Stop scheduling on a node and move its replicas elsewhere (redeploys the affected environments)

**Usage:** `nodes drain <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status` | Only show drain progress | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes undrain`

Let a drained node accept new deployments again

**Usage:** `nodes undrain <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes remove` (alias: `rm`)

Remove a drained node from the cluster and from every placement that targets it

**Usage:** `nodes remove <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--force` | Remove even if the node is not drained (its containers are not moved first) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes build-limits`

Show a worker node's build limits, or change them with flags

**Usage:** `nodes build-limits <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--timeout <seconds>` | Longest a build may run | - | No |
| `--concurrency <n>` | Builds that may run at once | - | No |
| `--queue-wait <seconds>` | Longest a build may wait for a slot | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes public-ingress`

Show or change whether a node serves public HTTP(S) traffic itself, and on which ports and addresses

**Usage:** `nodes public-ingress <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enable` | Serve public traffic on this node | - | No |
| `--disable` | Stop serving public traffic on this node | - | No |
| `--http-port <port>` | HTTP port to listen on | - | No |
| `--https-port <port>` | HTTPS port to listen on | - | No |
| `--listen-address <addr>` | Local address to bind ("" for automatic) | - | No |
| `--public-address <addr>` | Address DNS points at ("" for the listen address) | - | No |
| `--toggle-only` | Only flip enabled with --enable/--disable, without saving ports or addresses | - | No |
| `--check` | Check from the control plane that the node answers on its public address | - | No |
| `-y, --yes` | Do not ask before disabling | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes workload-mode`

Show an environment's workload mode, or set it (legacy, shadow, enforce); operator-only

**Usage:** `nodes workload-mode <environment-id> [mode] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask before changing it | - | No |
| `--json` | Output in JSON format | - | No |

## `tunnels`

Ingress tunnels: publish apps on workers with no open ports or certificates. Apps under a tunnel's domain go live by adding the hostname to an environment

**Subcommands:**

- `list` - List tunnels, what each publishes and the workers it runs on
- `workers` - List worker nodes, for choosing where a tunnel runs (create --node)
- `create` - Create a tunnel for a managed zone and run it on workers. The zone must already be managed by the DNS provider (dns-providers domains add); workers must allow ingress
- `hostnames` - List hostnames tunnels publish or could publish, with their state and why
- `explain` - Explain how a hostname reaches visitors: which tunnel carries it, or why none does
- `route` - Choose how a hostname reaches visitors: through a tunnel (--tunnel <id>), whichever tunnel covers it (--automatic), or no tunnel (--off)
- `publish` - Set what a tunnel publishes: every app under its domain (--all) or only chosen hostnames (--chosen)
- `nodes` - Set the workers a tunnel runs on (an empty list stops it everywhere)
- `links` - Show a tunnel's live connections to the provider: data centre, age, worker
- `sync` - Re-check a tunnel with its provider and recreate it if it was deleted there

### `tunnels list`

List tunnels, what each publishes and the workers it runs on

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <page>` | Page number (1-indexed) | `1` | No |
| `--page-size <pageSize>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

### `tunnels workers`

List worker nodes, for choosing where a tunnel runs (create --node)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `tunnels create`

Create a tunnel for a managed zone and run it on workers. The zone must already be managed by the DNS provider (dns-providers domains add); workers must allow ingress

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Lowercase letters, digits and hyphens | - | Yes |
| `--dns-provider <id>` | Cloudflare DNS provider id | - | Yes |
| `--domain <domain>` | Zone (or subdomain of one) whose apps it publishes | - | Yes |
| `--node <ids...>` | Worker node ids to run it on | - | Yes |
| `--provider <kind>` | Tunnel provider kind | `cloudflare_tunnel` | No |
| `--host-suffix <suffix>` | Name environment hosts <label>-<suffix>.<domain>, one level under the zone | - | No |
| `--secrets-file <path>` | JSON object of the provider's credential fields by key (- reads stdin) | - | No |
| `--json` | Output in JSON format | - | No |

### `tunnels hostnames`

List hostnames tunnels publish or could publish, with their state and why

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `tunnels explain`

Explain how a hostname reaches visitors: which tunnel carries it, or why none does

**Usage:** `tunnels explain <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `tunnels route`

Choose how a hostname reaches visitors: through a tunnel (--tunnel <id>), whichever tunnel covers it (--automatic), or no tunnel (--off)

**Usage:** `tunnels route <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--tunnel <id>` | Pin it to this tunnel | - | No |
| `--automatic` | Let the most specific covering tunnel publish it | - | No |
| `--off` | Keep every tunnel away from it (DNS managed elsewhere) | - | No |
| `--json` | Output in JSON format | - | No |

### `tunnels publish`

Set what a tunnel publishes: every app under its domain (--all) or only chosen hostnames (--chosen)

**Usage:** `tunnels publish <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--all` | Publish every app under its domain | - | No |
| `--chosen` | Publish only hostnames routed to it | - | No |

### `tunnels nodes`

Set the workers a tunnel runs on (an empty list stops it everywhere)

**Usage:** `tunnels nodes <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <ids...>` | Worker node ids | - | Yes |

### `tunnels links`

Show a tunnel's live connections to the provider: data centre, age, worker

**Usage:** `tunnels links <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `tunnels sync`

Re-check a tunnel with its provider and recreate it if it was deleted there

**Usage:** `tunnels sync <id>`

## `tokens` (alias: `token`)

Manage deployment tokens for project API access (KV, Blob, etc.)

**Subcommands:**

- `list` (`ls`) - List deployment tokens for a project
- `create` (`add`) - Create a new deployment token
- `show` (`get`) - Show deployment token details
- `delete` (`rm`) - Delete a deployment token
- `permissions` - List available deployment token permissions
- `deployment` - Deployment commands

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

### `tokens deployment`

Deployment commands

**Subcommands:**

- `list` - List all deployment tokens for a project
- `create` - Create a new deployment token for a project
- `delete` - Delete a deployment token
- `get` - Get a specific deployment token
- `update` - Update a deployment token
- `rotate` - Rotate a deployment token, invalidating its old secret and issuing a new one

#### `tokens deployment list`

List all deployment tokens for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment create`

Create a new deployment token for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--deployment-id <n>` | Optional deployment ID - if set, token is scoped to a specific deployment | - | No |
| `--environment-id <n>` | Optional environment ID - if not set, token applies to all environments | - | No |
| `--expires-at <value>` | Body field expires_at | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--permissions <value>` | List of permissions (e.g., ["visitors:enrich", "emails:send"]) If not provided, defaults to full access (repeatable) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment delete`

Delete a deployment token

**Usage:** `tokens deployment delete <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment get`

Get a specific deployment token

**Usage:** `tokens deployment get <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment update`

Update a deployment token

**Usage:** `tokens deployment update <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expires-at <value>` | Body field expires_at | - | No |
| `--is-active` | Body field is_active | - | No |
| `--no-is-active` | Set is_active to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--permissions <value>` | Body field permissions (repeatable) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment rotate`

Rotate a deployment token, invalidating its old secret and issuing a new one

**Usage:** `tokens deployment rotate <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

## `errors` (alias: `error`)

Manage error tracking and error groups

**Subcommands:**

- `list` (`ls`) - List error groups for a project
- `assign` - Assign an error group to a user or team, or unassign it
- `show` - Show error group details
- `update` - Update error group status
- `bulk` - Resolve, reopen or ignore many error groups in one request
- `merge` - Merge other error groups into one surviving group
- `unmerge` - Take an error group back out of the merge it is part of
- `split` - Split fingerprints out of an error group into a new one
- `fingerprints` - List the fingerprints routing to an error group
- `filters` - Drop noisy error events before they are stored (browser extensions, crawlers, localhost, message or release patterns)
- `comment` - Comment on an error group, or edit or delete a comment
- `activity` - Show the comments and history of an error group, newest first
- `searches` - Saved searches over error groups
- `performance` - Show whether traces are checked for performance issues, and what is detected
- `releases` - Show the health of each release: crash-free sessions and users, adoption and deployment
- `release` - Show one release's sessions and crash-free rate per hour
- `attachments` - Files SDKs send with errors and feedback, kept in blob storage
- `issues` - Issues that track an error group, in the project's repository or any tracker
- `commits` - Show the commits that last touched the code an error group fails in
- `ownership` - Rules that assign new error groups by the path of the failing code
- `events` - List events in an error group
- `event` - Show a specific error event
- `stats` - Get error statistics for a project
- `timeline` - Get error time series data
- `dashboard` - Get error dashboard statistics
- `sourcemaps` (`sm`) - Manage source maps for error symbolication
- `source-files` (`sf`) - Manage raw source files for native (Go/Rust/…) symbolication
- `list-global-groups` - List issues across every project visible to the caller
- `autofix` - Autofix commands
- `autofixer` - Autofixer commands
- `ingestion` - Ingestion commands
- `alert-rules` - Alert rules commands
- `search-group-assignees` - Search users and teams to assign an error group to
- `update-saved-search` - Rename, share or change the filters of your saved search
- `feedback` - Feedback commands
- `has-groups` - Check if project has any error groups

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
| `--assignee <who>` | Only groups assigned to: me (you or your teams), none, user:<id> or team:<id> | - | No |
| `--search <text>` | Only groups whose title, type or message contains this text | - | No |
| `--saved-search <id>` | Apply a saved search; flags given here override its filters | - | No |
| `--type <type>` | Only errors, or only performance issues found in traces: error or performance | - | No |
| `--json` | Output in JSON format | - | No |

### `errors assign`

Assign an error group to a user or team, or unassign it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--user-id <id>` | Assign to this user | - | No |
| `--team-id <id>` | Assign to this team | - | No |
| `--me` | Assign to yourself | - | No |
| `--none` | Unassign the group | - | No |
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
| `--assigned-to <email>` | Assign to the user with this email ("" unassigns); see also errors assign | - | No |
| `--ignore-for <duration>` | With ignore: come back after this long (30m, 12h, 7d, 2w) | - | No |
| `--ignore-until <date>` | With ignore: come back at this time (ISO 8601) | - | No |
| `--ignore-count <n>` | With ignore: come back after this many more occurrences | - | No |
| `--ignore-users <n>` | With ignore: come back once this many new users are affected | - | No |
| `--ignore-forever` | With ignore: never come back on its own | - | No |
| `--in-next-release` | With resolve: stay resolved until an event arrives from a release newer than the current one | - | No |

### `errors bulk`

Resolve, reopen or ignore many error groups in one request

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--action <action>` | Transition to apply (resolve, unresolve, ignore) | - | Yes |
| `--group-ids <ids>` | Comma-separated group IDs (at most 1000) | - | No |
| `--status <status>` | Select every group in this status instead of listing IDs | - | No |
| `--environment-id <id>` | Select every group of this environment | - | No |
| `--start-date <date>` | With --end-date: only groups with events in the window (ISO 8601) | - | No |
| `--end-date <date>` | With --start-date: only groups with events in the window (ISO 8601) | - | No |
| `--search <text>` | Select groups whose title, type or message contains this text | - | No |
| `--json` | Output in JSON format | - | No |
| `--ignore-for <duration>` | With ignore: come back after this long (30m, 12h, 7d, 2w) | - | No |
| `--ignore-until <date>` | With ignore: come back at this time (ISO 8601) | - | No |
| `--ignore-count <n>` | With ignore: come back after this many more occurrences | - | No |
| `--ignore-users <n>` | With ignore: come back once this many new users are affected | - | No |
| `--ignore-forever` | With ignore: never come back on its own | - | No |
| `--in-next-release` | With resolve: stay resolved until an event arrives from a release newer than the current one | - | No |

### `errors merge`

Merge other error groups into one surviving group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Surviving error group ID | - | Yes |
| `--source-group-ids <ids>` | Comma-separated group IDs to merge in (at most 50) | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors unmerge`

Take an error group back out of the merge it is part of

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Merged-away error group ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors split`

Split fingerprints out of an error group into a new one

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--fingerprints <hashes>` | Comma-separated fingerprint hashes to move out | - | Yes |
| `--title <title>` | Title for the new group | - | No |
| `--json` | Output in JSON format | - | No |

### `errors fingerprints`

List the fingerprints routing to an error group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors filters`

Drop noisy error events before they are stored (browser extensions, crawlers, localhost, message or release patterns)

**Subcommands:**

- `get` - Show one of a project's inbound filters
- `list` - List a project's inbound filters and how many events each dropped
- `add` - Add an inbound filter
- `update` - Change a filter's pattern, or switch it on or off
- `remove` (`delete`) - Delete an inbound filter; matching events are stored again

#### `errors filters get`

Show one of a project's inbound filters

**Usage:** `errors filters get <filter-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors filters list`

List a project's inbound filters and how many events each dropped

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors filters add`

Add an inbound filter

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--kind <kind>` | browser-extensions, web-crawlers, localhost, error-message or release | - | Yes |
| `--pattern <glob>` | For error-message and release: a case-insensitive glob, * and ? wildcards | - | No |
| `--disabled` | Create the filter switched off | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors filters update`

Change a filter's pattern, or switch it on or off

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--filter-id <id>` | Filter ID | - | Yes |
| `--pattern <glob>` | New pattern | - | No |
| `--enable` | Switch the filter on | - | No |
| `--disable` | Switch the filter off | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors filters remove` (alias: `delete`)

Delete an inbound filter; matching events are stored again

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--filter-id <id>` | Filter ID | - | Yes |

### `errors comment`

Comment on an error group, or edit or delete a comment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--body <text>` | The comment (at most 10,000 characters) | - | No |
| `--edit <comment-id>` | Replace the text of your comment with --body | - | No |
| `--delete <comment-id>` | Delete a comment (yours, or any as an administrator) | - | No |
| `--json` | Output in JSON format | - | No |

### `errors activity`

Show the comments and history of an error group, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--page <n>` | Page number | `1` | No |
| `--page-size <n>` | Entries per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

### `errors searches`

Saved searches over error groups

**Subcommands:**

- `list` (`ls`) - List your saved searches and the ones shared with the project
- `save` - Save a set of list filters under a name
- `delete` - Delete a saved search (yours, or a shared one as an administrator)

#### `errors searches list` (alias: `ls`)

List your saved searches and the ones shared with the project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors searches save`

Save a set of list filters under a name

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--name <name>` | Name, unique among your saved searches in the project | - | Yes |
| `--shared` | Share with everyone who can open the project | - | No |
| `--status <status>` | unresolved, resolved or ignored | - | No |
| `--environment-id <id>` | An environment of the project | - | No |
| `--assignee <who>` | me, none, user:<id> or team:<id> | - | No |
| `--search <text>` | Text in the title, type or message | - | No |
| `--sort-by <field>` | last_seen, first_seen, total_count or events_in_range | - | No |
| `--sort-order <order>` | asc or desc | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors searches delete`

Delete a saved search (yours, or a shared one as an administrator)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--id <id>` | Saved search ID | - | Yes |

### `errors performance`

Show whether traces are checked for performance issues, and what is detected

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors releases`

Show the health of each release: crash-free sessions and users, adoption and deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment <name>` | SDK environment (as the SDK sends it) | - | No |
| `--days <n>` | Window in days, 1 to 14 | `7` | No |
| `--page <n>` | Page | `1` | No |
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `list-telemetry` - List telemetry releases

#### `errors releases list-telemetry`

List telemetry releases

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--release <value>` | Exact SDK release identity; absent lists observed releases | - | No |
| `--days <n>` | Retained observation window, 1–31 days; default 7 | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `errors release`

Show one release's sessions and crash-free rate per hour

**Usage:** `errors release <release> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment <name>` | SDK environment (as the SDK sends it) | - | No |
| `--days <n>` | Window in days, 1 to 14 | `7` | No |
| `--json` | Output in JSON format | - | No |

### `errors attachments`

Files SDKs send with errors and feedback, kept in blob storage

**Subcommands:**

- `list` (`ls`) - List the attachments of an error event (--group-id with --event-id) or of a feedback (--feedback-id)
- `download` - Save an attachment to a file
- `delete` (`rm`) - Delete an attachment and return its space to the quota
- `usage` - Show whether attachments are kept and how much of the quota they use
- `quota` - Set how much attachment storage the project may use

#### `errors attachments list` (alias: `ls`)

List the attachments of an error event (--group-id with --event-id) or of a feedback (--feedback-id)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group of the event; give it with --event-id, not with --feedback-id | - | No |
| `--event-id <id>` | Error event ID; give it with --group-id, not with --feedback-id | - | No |
| `--feedback-id <id>` | Feedback ID; give it alone, instead of --group-id and --event-id | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors attachments download`

Save an attachment to a file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--attachment-id <id>` | Attachment ID | - | Yes |
| `-o, --output <path>` | File to write (default: the attachment name, in this directory) | - | No |
| `-f, --force` | Overwrite the file if it exists | - | No |

#### `errors attachments delete` (alias: `rm`)

Delete an attachment and return its space to the quota

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--attachment-id <id>` | Attachment ID | - | Yes |
| `-y, --yes` | Skip confirmation | - | No |

#### `errors attachments usage`

Show whether attachments are kept and how much of the quota they use

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors attachments quota`

Set how much attachment storage the project may use

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--mib <n>` | Quota in MiB, 0 to 102400 | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors issues`

Issues that track an error group, in the project's repository or any tracker

**Subcommands:**

- `list` (`ls`) - List the issues linked to an error group
- `open` - Open an issue for an error group in the project's GitHub or GitLab repository
- `link` - Link an existing issue from any tracker to an error group
- `unlink` - Unlink an issue from an error group; the issue itself is left as it is

#### `errors issues list` (alias: `ls`)

List the issues linked to an error group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--refresh` | Read again whether each GitHub or GitLab issue is open | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors issues open`

Open an issue for an error group in the project's GitHub or GitLab repository

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--title <title>` | Issue title (default: the group's title) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors issues link`

Link an existing issue from any tracker to an error group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--url <url>` | The issue URL | - | Yes |
| `--title <title>` | What to call it (default: the issue's title, or its URL) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors issues unlink`

Unlink an issue from an error group; the issue itself is left as it is

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--link-id <id>` | Issue link ID (from `temps errors issues list`) | - | Yes |
| `-y, --yes` | Skip confirmation | - | No |

### `errors commits`

Show the commits that last touched the code an error group fails in

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors ownership`

Rules that assign new error groups by the path of the failing code

**Subcommands:**

- `list` (`ls`) - List the project's ownership rules in the order they are tried
- `set` - Replace the ownership rules with those in a file: one "<pattern> <user-id>" per line

#### `errors ownership list` (alias: `ls`)

List the project's ownership rules in the order they are tried

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors ownership set`

Replace the ownership rules with those in a file: one "<pattern> <user-id>" per line

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--file <path>` | Rules file; "-" reads standard input | - | No |
| `--clear` | Remove every rule | - | No |
| `--json` | Output in JSON format | - | No |

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
| `--tag-key <value>` | Only events with this tag (with --tag-value) | - | No |
| `--tag-value <value>` | The value --tag-key must have | - | No |

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

### `errors list-global-groups`

List issues across every project visible to the caller

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--project-id <n>` | Query parameter project_id | - | No |
| `--status <value>` | Query parameter status | - | No |
| `--search <value>` | Query parameter search | - | No |
| `--start-date <value>` | Query parameter start_date | - | No |
| `--end-date <value>` | Query parameter end_date | - | No |
| `--json` | Output in JSON format | - | No |

### `errors autofix`

Autofix commands

**Subcommands:**

- `get-capability` - Whether autofix can run for the project, what a fix would become, and the project's autofix policy
- `list-incidents` - The project's autofix incidents, newest first
- `open-incident` - Ask autofix to reproduce and fix an error group
- `get-incident` - One autofix incident
- `close-incident` - Record the decision on a fix waiting for review
- `list-incident-events` - The incident's timeline: every action the server, the agent and people took, oldest first
- `download-patch` - The fix as a unified diff, for 'git apply'
- `stop-incident` - Stop an incident: cancels the agent's turn, deletes the sandbox and keeps the transcript and timeline
- `set-paused` - Pause or resume autofix for the project
- `update-settings` - Change the project's autofix policy

#### `errors autofix get-capability`

Whether autofix can run for the project, what a fix would become, and the project's autofix policy

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix list-incidents`

The project's autofix incidents, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number, from 1 | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--status <value>` | 'open', 'closed', or one exact status | - | No |
| `--error-group-id <n>` | Only incidents of this error group | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix open-incident`

Ask autofix to reproduce and fix an error group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--error-group-id <n>` | The error group to fix [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix get-incident`

One autofix incident

**Usage:** `errors autofix get-incident <incident-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix close-incident`

Record the decision on a fix waiting for review

**Usage:** `errors autofix close-incident <incident-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--decision <value>` | 'merged' (you merged the pull request), 'applied' (you applied the patch) or 'rejected' [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix list-incident-events`

The incident's timeline: every action the server, the agent and people took, oldest first

**Usage:** `errors autofix list-incident-events <incident-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--after-id <n>` | Only events after this id; pass the last id seen to poll | - | No |
| `--limit <n>` | Default 100, max 500 | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix download-patch`

The fix as a unified diff, for 'git apply'

**Usage:** `errors autofix download-patch <incident-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix stop-incident`

Stop an incident: cancels the agent's turn, deletes the sandbox and keeps the transcript and timeline

**Usage:** `errors autofix stop-incident <incident-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix set-paused`

Pause or resume autofix for the project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--paused` | Body field paused [required] | - | No |
| `--no-paused` | Set paused to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofix update-settings`

Change the project's autofix policy

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Empty string clears it | - | No |
| `--ai-provider <value>` | Empty string clears it (back to the assistant's default) | - | No |
| `--min-confidence <n>` | Confidence level, 0-5, an 'auto_merge' fix must reach | - | No |
| `--mode <value>` | 'off', 'propose' or 'auto_merge' | - | No |
| `--protected-paths <value>` | Body field protected_paths (repeatable) | - | No |
| `--test-command <value>` | Empty string clears it | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `errors autofixer`

Autofixer commands

**Subcommands:**

- `start-analysis` - Start an autofixer analysis run for the given error group
- `get-run` - Get a single autofixer run with its logs
- `add-context` - Append a user message to the run's context field
- `create-pr` - Push the fix branch and create a pull request
- `start-fix` - Transition from analysis to fix phase
- `re-analyze` - Continue the conversation with user feedback

#### `errors autofixer start-analysis`

Start an autofixer analysis run for the given error group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--branch <value>` | Branch to clone instead of the project's main branch | - | No |
| `--error-group-id <n>` | Body field error_group_id [required] | - | No |
| `--max-turns <n>` | Per-run turn cap applied to every phase (1–200) | - | No |
| `--model <value>` | Model id for the chosen provider. 'None' uses the provider's saved default model | - | No |
| `--provider <value>` | AI provider id ("claude_cli", "codex_cli", "opencode"). 'None' uses the platform default provider | - | No |
| `--user-context <value>` | Free-text notes for the model (extra context about the error, retry guidance, constraints) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofixer get-run`

Get a single autofixer run with its logs

**Usage:** `errors autofixer get-run <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofixer add-context`

Append a user message to the run's context field

**Usage:** `errors autofixer add-context <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--message <value>` | Body field message [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofixer create-pr`

Push the fix branch and create a pull request

**Usage:** `errors autofixer create-pr <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofixer start-fix`

Transition from analysis to fix phase

**Usage:** `errors autofixer start-fix <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors autofixer re-analyze`

Continue the conversation with user feedback

**Usage:** `errors autofixer re-analyze <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `errors ingestion`

Ingestion commands

**Subcommands:**

- `list-client-outcomes` - List client outcomes

#### `errors ingestion list-client-outcomes`

List client outcomes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--days <n>` | Retained observation window, 1–31 days; default 7 | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `errors alert-rules`

Alert rules commands

**Subcommands:**

- `list` - List all alert rules for a project
- `create` - Create a new alert rule
- `delete` - Delete an alert rule
- `get` - Get a specific alert rule
- `update` - Update an existing alert rule

#### `errors alert-rules list`

List all alert rules for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors alert-rules create`

Create a new alert rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--cooldown-minutes <n>` | Minimum minutes between notifications for same rule+group | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--environment-filter <n>` | Optional environment ID to filter alerts | - | No |
| `--error-level-filter <value>` | Optional error type/level filter | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--notification-priority <value>` | Notification priority: Low, Normal, High, Critical | - | No |
| `--trigger-config <json>` | Trigger-specific configuration (e.g., {"count": 100, "window_minutes": 60} for frequency) (JSON) | - | No |
| `--trigger-type <value>` | Trigger type: new_issue, regression, frequency, new_user, user_count, status_change [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors alert-rules delete`

Delete an alert rule

**Usage:** `errors alert-rules delete <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors alert-rules get`

Get a specific alert rule

**Usage:** `errors alert-rules get <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors alert-rules update`

Update an existing alert rule

**Usage:** `errors alert-rules update <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--cooldown-minutes <n>` | Body field cooldown_minutes | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--environment-filter <n>` | Body field environment_filter | - | No |
| `--error-level-filter <value>` | Body field error_level_filter | - | No |
| `--name <value>` | Body field name | - | No |
| `--notification-priority <value>` | Body field notification_priority | - | No |
| `--trigger-config <json>` | Body field trigger_config (JSON) | - | No |
| `--trigger-type <value>` | Body field trigger_type | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `errors search-group-assignees`

Search users and teams to assign an error group to

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--q <value>` | Text in the user's name or email, or the team's name | - | No |
| `--json` | Output in JSON format | - | No |

### `errors update-saved-search`

Rename, share or change the filters of your saved search

**Usage:** `errors update-saved-search <saved-search-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--filters <json>` | The list filters a saved search applies (JSON) | - | No |
| `--name <value>` | Body field name | - | No |
| `--shared` | Body field shared | - | No |
| `--no-shared` | Set shared to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `errors feedback`

Feedback commands

**Subcommands:**

- `list` - List project feedback
- `delete` - Delete project feedback
- `get` - Get project feedback
- `update` - Update project feedback

#### `errors feedback list`

List project feedback

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--status <value>` | Query parameter status | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors feedback delete`

Delete project feedback

**Usage:** `errors feedback delete <feedback-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors feedback get`

Get project feedback

**Usage:** `errors feedback get <feedback-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors feedback update`

Update project feedback

**Usage:** `errors feedback update <feedback-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--status <value>` | Body field status [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `errors has-groups`

Check if project has any error groups

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

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

## `server`

Resource usage of the machine running the Temps control plane (what /monitoring/server shows)

**Subcommands:**

- `status` - Latest CPU, memory, disk, block I/O and network I/O sample for the control-plane host
- `metrics` - Time series of one control-plane host metric, e.g. node.cpu_percent or node.network_rx_bytes_total
- `cluster-metrics` - Time series of one metric combined across nodes (counters summed, ratios averaged)
- `cluster-latest` - Latest value of every metric, combined across nodes
- `reporting` - Whether each worker reports metrics, and why not
- `disk-health` - Latest software RAID and drive SMART health for a node
- `alert-rules` - CPU, memory and disk alerts on a node
- `docker-disk-usage` (`df`) - Docker disk usage by images, containers, volumes and build cache (docker system df) on the control-plane host

### `server status`

Latest CPU, memory, disk, block I/O and network I/O sample for the control-plane host

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |

### `server metrics`

Time series of one control-plane host metric, e.g. node.cpu_percent or node.network_rx_bytes_total

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--metric <name>` | Metric name (node.cpu_percent, node.memory_used_bytes, node.disk_used_bytes, node.disk_read_bytes_total, node.network_tx_bytes_total, ...) | - | Yes |
| `--range <range>` | Time window: 1h, 6h, 24h, 7d | `1h` | No |
| `--start <time>` | Start of an explicit window (ISO 8601); with --end, instead of --range | - | No |
| `--end <time>` | End of an explicit window (ISO 8601) | - | No |
| `--percentile <p>` | Aggregate each bucket at this percentile (e.g. 0.95) instead of the mean | - | No |
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |

### `server cluster-metrics`

Time series of one metric combined across nodes (counters summed, ratios averaged)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--metric <name>` | Metric name, e.g. node.cpu_percent | - | Yes |
| `--nodes <ids>` | Comma-separated node IDs, e.g. 0,1,2 | - | Yes |
| `--range <range>` | Time window, e.g. 1h, 24h, 7d | - | No |
| `--start <time>` | Start of an explicit window (ISO 8601) | - | No |
| `--end <time>` | End of an explicit window (ISO 8601) | - | No |
| `--json` | Output in JSON format | - | No |

### `server cluster-latest`

Latest value of every metric, combined across nodes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--nodes <ids>` | Comma-separated node IDs | - | Yes |
| `--json` | Output in JSON format | - | No |

### `server reporting`

Whether each worker reports metrics, and why not

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `server disk-health`

Latest software RAID and drive SMART health for a node

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |

### `server alert-rules`

CPU, memory and disk alerts on a node

**Subcommands:**

- `get` - Show one of a node's resource alerts
- `create` - Alert when a node metric crosses a threshold
- `delete` (`rm`) - Delete a node's resource alert

#### `server alert-rules get`

Show one of a node's resource alerts

**Usage:** `server alert-rules get <id> <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `server alert-rules create`

Alert when a node metric crosses a threshold

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--name <name>` | Rule name | - | Yes |
| `--metric <name>` | Metric, e.g. node.cpu_percent or node.disk_used_percent | - | Yes |
| `--comparator <op>` | One of >, <, >=, <=, or gt, lt, gte, lte | - | Yes |
| `--threshold <value>` | Threshold value | - | Yes |
| `--severity <level>` | One of warning, critical | `warning` | No |
| `--for <seconds>` | How long the condition must hold before it fires | - | No |
| `--disabled` | Create it turned off | - | No |
| `--json` | Output in JSON format | - | No |

#### `server alert-rules delete` (alias: `rm`)

Delete a node's resource alert

**Usage:** `server alert-rules delete <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `-y, --yes` | Do not ask for confirmation | - | No |

### `server docker-disk-usage` (alias: `df`)

Docker disk usage by images, containers, volumes and build cache (docker system df) on the control-plane host

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |

## `traces` (alias: `trace`)

Inspect distributed traces and operation latency

**Subcommands:**

- `span-stats` (`operations`, `ops`) - Rank operations by time spent, latency percentiles, or inconsistency
- `trend` - Show how one operation's latency moved, day by day, with deployments
- `regressions` - List the operations that got slower or faster after a deployment
- `breakdown` - Show where the time inside one operation goes: queries, downstream calls, its own code
- `page` - Compare a page's web vitals with the backend requests that served it
- `show` (`get`) - Show every span of one trace: timing, service, status, attributes and events
- `query-global` - Query global traces
- `query-global-summaries` - Query global trace summaries
- `get-unified` - Assemble a unified cross-project span waterfall (Phase 2)
- `query-summaries` - Query trace summaries — one row per trace with span count, error count, root span info, and proper..
- `query` - Query trace spans with optional filters
- `get-cross-siblings` - Discover sibling projects that share the same 'trace_id' (Phase 1 banner)

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
| `--apdex-ms <ms>` | Apdex threshold: calls at or under this satisfy, up to 4x tolerate (default 500) | - | No |
| `--sort-by <field>` | Ranking (total_time, p50, p95, p99, max, avg, stddev, count, errors, error_rate, variability, tail_ratio) | `total_time` | No |
| `--sort-order <order>` | asc or desc | `desc` | No |
| `--limit <n>` | Rows to show (max 100) | `20` | No |
| `--offset <n>` | Page offset | - | No |
| `--json` | Output in JSON format | - | No |

### `traces trend`

Show how one operation's latency moved, day by day, with deployments

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--service <name>` | Service of the operation | - | No |
| `--operation <name>` | The operation (span name) | - | No |
| `--days <n>` | Days to show (max 30) | `14` | No |
| `--json` | Output the hourly points in JSON format | - | No |

### `traces regressions`

List the operations that got slower or faster after a deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--deployment-id <id>` | Deployment to compare across | - | No |
| `--json` | Output in JSON format | - | No |

### `traces breakdown`

Show where the time inside one operation goes: queries, downstream calls, its own code

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--service <name>` | Service of the operation | - | No |
| `--operation <name>` | The operation (span name) | - | No |
| `--sample <kind>` | Calls to sample (recent, slow) | `recent` | No |
| `--attribute <key>` | Also split the calls by this attribute, e.g. http.route | - | No |
| `--json` | Output in JSON format | - | No |

### `traces page`

Compare a page's web vitals with the backend requests that served it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--path <path>` | The page path, e.g. /checkout | - | No |
| `--json` | Output in JSON format | - | No |

### `traces show` (alias: `get`)

Show every span of one trace: timing, service, status, attributes and events

**Usage:** `traces show <trace-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project the trace belongs to | - | Yes |
| `--start-time <iso>` | Look for the trace from this time (ISO 8601); pair with --end-time | - | No |
| `--end-time <iso>` | Look for the trace up to this time (ISO 8601); pair with --start-time | - | No |
| `--json` | Output the spans in JSON format | - | No |

### `traces query-global`

Query global traces

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Query parameter project_id | - | No |
| `--trace-id <value>` | Query parameter trace_id | - | No |
| `--service-name <value>` | Query parameter service_name | - | No |
| `--status <value>` | Query parameter status | - | No |
| `--min-duration-ms <number>` | Query parameter min_duration_ms | - | No |
| `--start-time <value>` | Query parameter start_time | - | No |
| `--end-time <value>` | Query parameter end_time | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--deployment-id <n>` | Query parameter deployment_id | - | No |
| `--attributes <value>` | Filter by span attributes as comma-separated key=value pairs. e.g.... | - | No |
| `--name-pattern <value>` | Filter by span name pattern (ILIKE) | - | No |
| `--sort-by <value>` | Sort field for the trace-summaries list: "start_time" (default) or "duration" | - | No |
| `--sort-order <value>` | Sort direction: "asc" or "desc" (default) | - | No |
| `--limit <n>` | Query parameter limit | - | No |
| `--offset <n>` | Query parameter offset | - | No |
| `--json` | Output in JSON format | - | No |

### `traces query-global-summaries`

Query global trace summaries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Query parameter project_id | - | No |
| `--trace-id <value>` | Query parameter trace_id | - | No |
| `--service-name <value>` | Query parameter service_name | - | No |
| `--status <value>` | Query parameter status | - | No |
| `--min-duration-ms <number>` | Query parameter min_duration_ms | - | No |
| `--start-time <value>` | Query parameter start_time | - | No |
| `--end-time <value>` | Query parameter end_time | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--deployment-id <n>` | Query parameter deployment_id | - | No |
| `--attributes <value>` | Filter by span attributes as comma-separated key=value pairs. e.g.... | - | No |
| `--name-pattern <value>` | Filter by span name pattern (ILIKE) | - | No |
| `--sort-by <value>` | Sort field for the trace-summaries list: "start_time" (default) or "duration" | - | No |
| `--sort-order <value>` | Sort direction: "asc" or "desc" (default) | - | No |
| `--limit <n>` | Query parameter limit | - | No |
| `--offset <n>` | Query parameter offset | - | No |
| `--json` | Output in JSON format | - | No |

### `traces get-unified`

Assemble a unified cross-project span waterfall (Phase 2)

**Usage:** `traces get-unified <trace-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-time <value>` | Window start (RFC 3339); pair with end_time, max 31 days | - | No |
| `--end-time <value>` | Window end (RFC 3339); pair with start_time, max 31 days | - | No |
| `--json` | Output in JSON format | - | No |

### `traces query-summaries`

Query trace summaries — one row per trace with span count, error count, root span info, and proper..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--trace-id <value>` | Filter by trace ID | - | No |
| `--service-name <value>` | Filter by service name | - | No |
| `--status <value>` | Filter by status (OK, ERROR) | - | No |
| `--min-duration-ms <number>` | Minimum trace duration in ms | - | No |
| `--start-time <value>` | Start time (RFC 3339) | - | No |
| `--end-time <value>` | End time (RFC 3339) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--deployment-id <n>` | Filter by deployment ID | - | No |
| `--attributes <value>` | Filter by span attributes as comma-separated key=value pairs, e.g.... | - | No |
| `--name-pattern <value>` | Filter by span name pattern (ILIKE) | - | No |
| `--sort-by <value>` | Sort field: 'start_time' (default) or 'duration' | - | No |
| `--sort-order <value>` | Sort direction: 'asc' or 'desc' (default) | - | No |
| `--include-total` | Compute the 'total' count (default: true) | - | No |
| `--no-include-total` | Set include_total to false | - | No |
| `--limit <n>` | Max traces to return (default: 50, max: 100) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

### `traces query`

Query trace spans with optional filters

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--trace-id <value>` | Filter by trace ID | - | No |
| `--service-name <value>` | Filter by service name | - | No |
| `--status <value>` | Filter by status (OK, ERROR, UNSET) | - | No |
| `--min-duration-ms <number>` | Minimum span duration in ms | - | No |
| `--start-time <value>` | Start time (RFC 3339) | - | No |
| `--end-time <value>` | End time (RFC 3339) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--deployment-id <n>` | Filter by deployment ID | - | No |
| `--attributes <value>` | Filter by span attributes as comma-separated key=value pairs, e.g.... | - | No |
| `--name-pattern <value>` | Filter by span name pattern (ILIKE) | - | No |
| `--limit <n>` | Max spans to return (default: 100, max: 1000) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

### `traces get-cross-siblings`

Discover sibling projects that share the same 'trace_id' (Phase 1 banner)

**Usage:** `traces get-cross-siblings <trace-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--exclude-project-id <n>` | Project ID to exclude (the caller's own project) so the UI does not render a self-link | - | No |
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

**Usage:** `facets create <attribute-key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `facets remove` (alias: `rm`)

Remove a registered facet, freeing its slot for reuse

**Usage:** `facets remove <attribute-key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `facets retry`

Retry a failed historical backfill. Only valid when the facet's status is "failed" — resets progress and lets the background poller re-attempt from the beginning.

**Usage:** `facets retry <attribute-key> [options]`

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

**Usage:** `otel-forward show <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `otel-forward update`

Update an OTel forwarding destination

**Usage:** `otel-forward update <id> [options]`

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

**Usage:** `otel-forward remove <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `otel-forward test`

Send a test delivery to an OTel forwarding destination

**Usage:** `otel-forward test <id> [options]`

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

**Usage:** `otel-forward instance-default show <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default update`

Update an instance default forwarding destination

**Usage:** `otel-forward instance-default update <id> [options]`

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

**Usage:** `otel-forward instance-default remove <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `otel-forward instance-default test`

Send a test delivery to an instance default forwarding destination

**Usage:** `otel-forward instance-default test <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `otel`

Inspect the OTLP ingest pipeline itself — throughput, drops and failure reasons (server-wide, not project-scoped; see "temps metrics" to query ingested application metrics)

**Subcommands:**

- `ingest-errors` - Show why ingest batches were dropped, grouped by signal and failure reason
- `pipeline-history` - Show pipeline counter trends over time (received/stored/dropped per signal)
- `alerts` - Alerts commands
- `get-cloud-backfill-status` - Status of the most recent Temps Cloud telemetry backfill for a project
- `dashboards` - Dashboards commands
- `genai` - Genai commands
- `has-traces` - Whether a project has ever received at least one trace span
- `get-health` - Get health summaries for a project
- `insights` - Insights commands
- `logs` - Logs commands
- `get-pipeline-stats` - Get OTel pipeline statistics (admin/system view)
- `get-quota` - Get storage quota for a project
- `observe` - Observe commands

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

### `otel alerts`

Alerts commands

**Subcommands:**

- `list` - List alert rules for a project (newest first, paginated)
- `create` - Create a new alert rule for a project
- `preview` - Backtest an anomaly detector over a time range without saving a rule
- `delete` - Delete an alert rule
- `get` - Fetch a single alert rule by id
- `update` - Update an alert rule's fields

#### `otel alerts list`

List alert rules for a project (newest first, paginated)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts create`

Create a new alert rule for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--aggregation <value>` | One of 'avg\|sum\|min\|max\|count\|rate\|p50\|p90\|p95\|p99' [required] | - | No |
| `--detection-config <json>` | The detector: a discriminated union keyed by 'kind' (JSON) [required] | - | No |
| `--dynamic-alerts` | When true (and 'group_by' is set) fire one independent alarm per breaching series | - | No |
| `--no-dynamic-alerts` | Set dynamic_alerts to false | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--for-duration-secs <n>` | Body field for_duration_secs [required] | - | No |
| `--group-by <value>` | Label keys to break the metric down by, e.g. '["endpoint","region"]' (repeatable) | - | No |
| `--grouped-notification-threshold <n>` | When more than this many series transition to firing in the same tick, only the first gets the expensive... | - | No |
| `--label-filters <json>` | AND-combined label equality filters: '[["key","value"],…]' (JSON) | - | No |
| `--max-series <n>` | Cardinality cap for dynamic alerting: at most this many series (top by '\|value\|') | - | No |
| `--metric-name <value>` | The OTel metric to watch | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--project-id <n>` | Body field project_id [required] | - | No |
| `--severity <value>` | One of 'info\|warning\|critical' [required] | - | No |
| `--source <json>` | What the rule watches: the metric above (the default), or the number of log lines matching a saved search... (JSON) | - | No |
| `--window-secs <n>` | Body field window_secs [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts preview`

Backtest an anomaly detector over a time range without saving a rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--aggregation <value>` | One of 'avg\|sum\|min\|max\|count\|rate\|p50\|p90\|p95\|p99' [required] | - | No |
| `--detection-config <json>` | The detector to backtest. 'static' and 'anomaly' are supported — the kinds the evaluator actually runs (JSON) [required] | - | No |
| `--end-time <value>` | RFC 3339; defaults to now | - | No |
| `--metric-name <value>` | Body field metric_name [required] | - | No |
| `--project-id <n>` | Body field project_id [required] | - | No |
| `--start-time <value>` | RFC 3339; defaults to 7 days before 'end_time' | - | No |
| `--window-secs <n>` | Body field window_secs [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts delete`

Delete an alert rule

**Usage:** `otel alerts delete <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the delete) [required] | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts get`

Fetch a single alert rule by id

**Usage:** `otel alerts get <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the lookup) [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts update`

Update an alert rule's fields

**Usage:** `otel alerts update <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the update) [required] | - | No |
| `--aggregation <value>` | Body field aggregation | - | No |
| `--detection-config <json>` | The typed detector definition stored (as jsonb) in 'metric_alert_rules.detection_config' (JSON) | - | No |
| `--dynamic-alerts` | Toggles per-series ("dynamic") alerting (absent = leave unchanged) | - | No |
| `--no-dynamic-alerts` | Set dynamic_alerts to false | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--for-duration-secs <n>` | Body field for_duration_secs | - | No |
| `--group-by <value>` | Replaces the group_by keys wholesale when present (absent = leave unchanged) (repeatable) | - | No |
| `--grouped-notification-threshold <n>` | Updates the notification-grouping threshold (absent = leave unchanged) | - | No |
| `--label-filters <json>` | Replaces the label filters wholesale when present (absent = leave unchanged) (JSON) | - | No |
| `--max-series <n>` | Updates the dynamic-alerting cardinality cap (absent = leave unchanged) | - | No |
| `--metric-name <value>` | Body field metric_name | - | No |
| `--name <value>` | Body field name | - | No |
| `--severity <value>` | Body field severity | - | No |
| `--source <json>` | What series a rule is evaluated on (ADR 108 D1) (JSON) | - | No |
| `--window-secs <n>` | Body field window_secs | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel get-cloud-backfill-status`

Status of the most recent Temps Cloud telemetry backfill for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel dashboards`

Dashboards commands

**Subcommands:**

- `list` - List dashboards for a project (newest first, paginated)
- `create` - Create a new dashboard for a project
- `delete` - Delete a dashboard
- `get` - Fetch a single dashboard by id
- `update` - Update a dashboard's name and/or layout

#### `otel dashboards list`

List dashboards for a project (newest first, paginated)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel dashboards create`

Create a new dashboard for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--layout <json>` | The typed layout persisted (as JSONB) in 'metric_dashboards.layout' (JSON) [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--project-id <n>` | Body field project_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel dashboards delete`

Delete a dashboard

**Usage:** `otel dashboards delete <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the delete) [required] | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel dashboards get`

Fetch a single dashboard by id

**Usage:** `otel dashboards get <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the lookup) [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel dashboards update`

Update a dashboard's name and/or layout

**Usage:** `otel dashboards update <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the update) [required] | - | No |
| `--layout <json>` | The typed layout persisted (as JSONB) in 'metric_dashboards.layout' (JSON) | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel genai`

Genai commands

**Subcommands:**

- `query-traces` - Query GenAI trace summaries — traces containing spans with 'gen_ai.*' attributes
- `get-trace` - Get GenAI span details for a specific trace

#### `otel genai query-traces`

Query GenAI trace summaries — traces containing spans with 'gen_ai.*' attributes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--service-name <value>` | Filter by service name | - | No |
| `--gen-ai-system <value>` | Filter by AI system (openai, anthropic, etc.) | - | No |
| `--gen-ai-model <value>` | Filter by model (gpt-6.2-sol, claude-sonnet-5-5, etc.) | - | No |
| `--start-time <value>` | Start time (RFC 3339) | - | No |
| `--end-time <value>` | End time (RFC 3339) | - | No |
| `--limit <n>` | Max traces to return (default: 50, max: 100) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel genai get-trace`

Get GenAI span details for a specific trace

**Usage:** `otel genai get-trace <trace-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-time <value>` | Window start (RFC 3339); pair with end_time, max 31 days | - | No |
| `--end-time <value>` | Window end (RFC 3339); pair with start_time, max 31 days | - | No |
| `--json` | Output in JSON format | - | No |

### `otel has-traces`

Whether a project has ever received at least one trace span

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel get-health`

Get health summaries for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `otel insights`

Insights commands

**Subcommands:**

- `list` - List anomaly insights for a project

#### `otel insights list`

List anomaly insights for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--status <value>` | Filter by status (active, resolved) | - | No |
| `--limit <n>` | Max insights to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

### `otel logs`

Logs commands

**Subcommands:**

- `query` - Query log records with optional filters

#### `otel logs query`

Query log records with optional filters

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--severity <value>` | Filter by severity (TRACE, DEBUG, INFO, WARN, ERROR, FATAL) | - | No |
| `--service-name <value>` | Filter by service name | - | No |
| `--search <value>` | Full-text search in log body (ILIKE) | - | No |
| `--attribute-key <value>` | Exact attribute key; pair with attribute_value | - | No |
| `--attribute-value <value>` | Exact string or numeric text, preserving int64 precision | - | No |
| `--attribute-type <value>` | Optional native declared type; older records without type metadata do not match (one of: string, boolean, integer, double, array, object, bytes) | - | No |
| `--attribute-op <value>` | Default eq; numeric types also support lt/lte/gt/gte (one of: eq, ne, lt, lte, gt, gte) | - | No |
| `--trace-id <value>` | Filter by correlated trace ID | - | No |
| `--start-time <value>` | Start time (RFC 3339) | - | No |
| `--end-time <value>` | End time (RFC 3339) | - | No |
| `--limit <n>` | Max logs to return (default: 100, max: 1000) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

### `otel get-pipeline-stats`

Get OTel pipeline statistics (admin/system view)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `otel get-quota`

Get storage quota for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel observe`

Observe commands

**Subcommands:**

- `observability-list-events` - List a merged page of observability events for a project
- `observability-full-event` - Fetch the un-truncated form of one event by '(kind, id)'

#### `otel observe observability-list-events`

List a merged page of observability events for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--kinds <value>` | Comma-separated kinds: 'request,span,error,revenue' | - | No |
| `--from <value>` | Inclusive lower bound on event timestamp (ISO 8601, 'Z' suffix) | - | No |
| `--to <value>` | Inclusive upper bound on event timestamp | - | No |
| `--deployment-id <n>` | Query parameter deployment_id | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--search <value>` | Free-text substring matched against per-kind summary fields (request path / error class / revenue event_type) | - | No |
| `--limit <n>` | Page size (default 50, max 200) | - | No |
| `--hide-bots` | When 'true', exclude bot/crawler request rows | - | No |
| `--no-hide-bots` | Set hide_bots to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel observe observability-full-event`

Fetch the un-truncated form of one event by '(kind, id)'

**Usage:** `otel observe observability-full-event <kind> <event-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ts <value>` | The row's event timestamp as returned by the list endpoint | - | No |
| `--json` | Output in JSON format | - | No |

## `kv`

Read and write a project's keys in the KV store, and manage the KV service

**Subcommands:**

- `get` - Get the value of a key
- `set` - Set a key; the value is a string unless --json-value
- `del` (`delete`) - Delete one or more keys
- `keys` (`ls`) - List keys matching a pattern (* and ? wildcards; default *)
- `ttl` - Show how long until a key expires
- `expire` - Expire a key after a number of seconds
- `incr` - Increment a numeric key (creating it at 0 first)
- `status` - Show whether the KV service is enabled and healthy on this instance
- `enable` - Enable the KV service on this instance (starts its Redis container)
- `update` - Change the Docker image the KV service runs (recreates its container)
- `disable` - Disable the KV service on this instance (stops its container; every project loses KV access)
- `container-name` - Show, or set with --container-name/--reset, the Docker container name of the KV service

### `kv get`

Get the value of a key

**Usage:** `kv get <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv set`

Set a key; the value is a string unless --json-value

**Usage:** `kv set <key> [value] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json-value` | Parse the value as JSON (a number, object, array, true/false or null) | - | No |
| `--ex <seconds>` | Expire the key after this many seconds | - | No |
| `--px <milliseconds>` | Expire the key after this many milliseconds | - | No |
| `--nx` | Only set the key if it does not exist | - | No |
| `--xx` | Only set the key if it already exists | - | No |
| `--json` | Output in JSON format | - | No |
| `--value-file <path>` | Read the value from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the value from stdin, e.g. piped from a secret manager | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `kv del` (alias: `delete`)

Delete one or more keys

**Usage:** `kv del <keys...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `kv keys` (alias: `ls`)

List keys matching a pattern (* and ? wildcards; default *)

**Usage:** `kv keys [pattern] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv ttl`

Show how long until a key expires

**Usage:** `kv ttl <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv expire`

Expire a key after a number of seconds

**Usage:** `kv expire <key> <seconds> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv incr`

Increment a numeric key (creating it at 0 first)

**Usage:** `kv incr <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--by <amount>` | Amount to add, may be negative (default 1) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv status`

Show whether the KV service is enabled and healthy on this instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `kv enable`

Enable the KV service on this instance (starts its Redis container)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Docker image to run (default: the one this Temps release pins) | - | No |
| `--max-memory <size>` | Memory limit, e.g. 256mb or 1gb | - | No |
| `--persistence` | Persist data to disk | - | No |
| `--no-persistence` | Keep data in memory only | - | No |
| `--json` | Output in JSON format | - | No |

### `kv update`

Change the Docker image the KV service runs (recreates its container)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Docker image to run | - | Yes |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `kv disable`

Disable the KV service on this instance (stops its container; every project loses KV access)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `kv container-name`

Show, or set with --container-name/--reset, the Docker container name of the KV service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--container-name <name>` | New container name (must keep the required prefix) | - | No |
| `--reset` | Go back to the default container name | - | No |
| `--json` | Output in JSON format | - | No |

## `flags` (alias: `flag`)

Manage feature flags (runtime config that changes without a redeploy)

**Subcommands:**

- `list` (`ls`) - List feature flags
- `get` - Show a feature flag and its per-environment values
- `create` - Create a feature flag
- `update` - Update a flag definition (default value, description, visibility)
- `rules` - Targeting rules: serve a different value to some users, or to a percentage of them
- `evaluate` (`eval`) - Show what a user would get, and why, in one environment
- `reshuffle` - Draw new rollout buckets for every rollout and split of this flag (never during an experiment)
- `set` - Set a flag value in one environment
- `clear` - Clear a flag override so the environment inherits the default
- `disable` - Kill switch: serve the default in this environment, ignoring any override
- `enable` - Re-enable a flag in this environment after a kill switch
- `restore` - Restore an archived flag
- `archive` - Archive a flag (callers fall back to their own default)
- `record-exposure` - Record which flags a running app actually evaluated
- `get-snapshot` - Every flag for the caller's environment, collapsed to what the evaluator needs
- `ofrep-server-bulk` - OFREP bulk evaluation for server-side OpenFeature SDKs
- `ofrep-server-single` - OFREP single-flag evaluation for server-side OpenFeature SDKs

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

**Usage:** `flags get <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `flags create`

Create a feature flag

**Usage:** `flags create <key> [options]`

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

**Usage:** `flags update <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-d, --default <value>` | New default value | - | No |
| `--description <text>` | New description | - | No |
| `--client-visible` | Expose this flag to browsers | - | No |
| `--no-client-visible` | Make this flag server-only | - | No |
| `--variants <json\|@file>` | Replace the variants, as a JSON array of {key, value, description?} (or @path to a file) | - | No |
| `--exposure-rate <percent>` | Share of visitors (0-100) whose browser records a flag_exposure event; 0 turns it off | - | No |
| `--json` | Output in JSON format | - | No |

### `flags rules`

Targeting rules: serve a different value to some users, or to a percentage of them

**Subcommands:**

- `list` (`ls`) - Show the ordered rules of a flag in one environment
- `set` - Replace the rules of a flag in one environment, from a JSON array (or @path to a file, or - for stdin)
- `clear` - Remove every rule of a flag in one environment

#### `flags rules list` (alias: `ls`)

Show the ordered rules of a flag in one environment

**Usage:** `flags rules list <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `--json` | Output the rules as JSON (the format `rules set` reads) | - | No |

#### `flags rules set`

Replace the rules of a flag in one environment, from a JSON array (or @path to a file, or - for stdin)

**Usage:** `flags rules set <key> <rules> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `flags rules clear`

Remove every rule of a flag in one environment

**Usage:** `flags rules clear <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags evaluate` (alias: `eval`)

Show what a user would get, and why, in one environment

**Usage:** `flags evaluate <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `-s, --subject <key>` | Bucketing subject, e.g. a user id | - | No |
| `-a, --attr <name=value>` | Targeting attribute (repeatable) | `` | No |
| `--json` | Output in JSON format | - | No |

### `flags reshuffle`

Draw new rollout buckets for every rollout and split of this flag (never during an experiment)

**Usage:** `flags reshuffle <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-y, --yes` | Skip the confirmation | - | No |

### `flags set`

Set a flag value in one environment

**Usage:** `flags set <key> <value> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `--json` | Output in JSON format | - | No |

### `flags clear`

Clear a flag override so the environment inherits the default

**Usage:** `flags clear <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags disable`

Kill switch: serve the default in this environment, ignoring any override

**Usage:** `flags disable <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags enable`

Re-enable a flag in this environment after a kill switch

**Usage:** `flags enable <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags restore`

Restore an archived flag

**Usage:** `flags restore <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `flags archive`

Archive a flag (callers fall back to their own default)

**Usage:** `flags archive <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `flags record-exposure`

Record which flags a running app actually evaluated

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--keys <value>` | Flag keys evaluated since the last report (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `flags get-snapshot`

Every flag for the caller's environment, collapsed to what the evaluator needs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--environment-id <n>` | Required only when the calling token is project-wide rather than scoped to a single environment | - | No |
| `--json` | Output in JSON format | - | No |

### `flags ofrep-server-bulk`

OFREP bulk evaluation for server-side OpenFeature SDKs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--context <json>` | Body field context (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `flags ofrep-server-single`

OFREP single-flag evaluation for server-side OpenFeature SDKs

**Usage:** `flags ofrep-server-single <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--context <json>` | Body field context (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

## `surveys` (alias: `survey`)

Manage in-app surveys (NPS, ratings, choices, open text) and read their answers

**Subcommands:**

- `list` (`ls`) - List surveys
- `get` - Show a survey: its questions and when it is shown
- `create` - Create a survey (a draft unless --status active)
- `update` - Change a survey; only the options given are changed
- `launch` - Start showing a survey to visitors
- `pause` - Stop showing a survey for now
- `complete` - Stop showing a survey for good (its results stay)
- `delete` - Delete a survey (answers already recorded stay in analytics events)
- `results` - Summarise the answers: counts, averages and NPS per question
- `responses` - List individual responses, newest first

### `surveys list` (alias: `ls`)

List surveys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--status <status>` | Only surveys in this status: draft, active, paused, completed | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys get`

Show a survey: its questions and when it is shown

**Usage:** `surveys get <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys create`

Create a survey (a draft unless --status active)

**Usage:** `surveys create <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-q, --questions <json\|@file\|->` | Questions as a JSON array of {id, kind, prompt, optional?, choices?, scale?, lower_label?, upper_label?} | - | Yes |
| `-t, --targeting <json\|@file\|->` | When to show it: {path_pattern?, trigger_event?, sample_rate?, once_per_visitor?, flag?: {key, value}} | - | No |
| `--description <text>` | Internal description | - | No |
| `--status <status>` | draft (default) or active | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys update`

Change a survey; only the options given are changed

**Usage:** `surveys update <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--name <name>` | New name | - | No |
| `--description <text>` | New description ("" clears it) | - | No |
| `-q, --questions <json\|@file\|->` | Replace the questions | - | No |
| `-t, --targeting <json\|@file\|->` | Replace when it is shown | - | No |
| `--status <status>` | draft, active, paused or completed | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys launch`

Start showing a survey to visitors

**Usage:** `surveys launch <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `surveys pause`

Stop showing a survey for now

**Usage:** `surveys pause <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `surveys complete`

Stop showing a survey for good (its results stay)

**Usage:** `surveys complete <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `surveys delete`

Delete a survey (answers already recorded stay in analytics events)

**Usage:** `surveys delete <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-y, --yes` | Skip the confirmation | - | No |

### `surveys results`

Summarise the answers: counts, averages and NPS per question

**Usage:** `surveys results <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Only answers from this environment | - | No |
| `--days <n>` | Look back this many days (default: 30, max: 366) | - | No |
| `--json` | Output in JSON format | - | No |

### `surveys responses`

List individual responses, newest first

**Usage:** `surveys responses <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Only answers from this environment | - | No |
| `--days <n>` | Look back this many days (default: 30, max: 366) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

## `data`

Browse the data inside a service (tables, collections, keys, objects) — read-only

**Subcommands:**

- `info` - Show what a service supports and how its containers nest
- `containers` (`databases`) - List top-level containers (databases, or buckets for S3)
- `tables` (`entities`) - List tables, collections, keys or objects in a container
- `schema` (`columns`) - Show an entity's columns, types and row count
- `rows` (`select`) - Read rows from an entity

### `data info`

Show what a service supports and how its containers nest

**Usage:** `data info <service> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `data containers` (alias: `databases`)

List top-level containers (databases, or buckets for S3)

**Usage:** `data containers <service> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | List containers nested under this path instead of the root | - | No |
| `--json` | Output in JSON format | - | No |

### `data tables` (alias: `entities`)

List tables, collections, keys or objects in a container

**Usage:** `data tables <service> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Container path, slash-separated (e.g. mydb/public) | - | Yes |
| `--limit <n>` | Maximum entities to return (default: 100) | - | No |
| `--next-token <token>` | Continue from the next_token of a previous page | - | No |
| `--json` | Output in JSON format | - | No |

### `data schema` (alias: `columns`)

Show an entity's columns, types and row count

**Usage:** `data schema <service> <entity> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Container path, slash-separated (e.g. mydb/public) | - | Yes |
| `--json` | Output in JSON format | - | No |

### `data rows` (alias: `select`)

Read rows from an entity

**Usage:** `data rows <service> <entity> [options]`

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

## `blob`

Upload, download and list a project's files in Blob storage, and manage the Blob service

**Subcommands:**

- `list` (`ls`) - List blobs, optionally under a pathname prefix
- `put` (`upload`) - Upload a local file
- `get` (`download`) - Download a blob to a file, or to stdout with -o -
- `head` (`stat`) - Show a blob's metadata (size, content type, last modified) without downloading it
- `copy` (`cp`) - Copy a blob (by pathname or URL) to a new pathname
- `del` (`delete`) - Delete one or more blobs
- `status` - Show whether the Blob service is enabled and healthy on this instance
- `enable` - Enable the Blob service on this instance (starts its S3-compatible container)
- `update` - Change the Docker image the Blob service runs (recreates its container)
- `disable` - Disable the Blob service on this instance (stops its container; every project loses Blob access)
- `container-name` - Show, or set with --container-name/--reset, the Docker container name of the Blob service

### `blob list` (alias: `ls`)

List blobs, optionally under a pathname prefix

**Usage:** `blob list [prefix] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--limit <n>` | Blobs per page | - | No |
| `--cursor <token>` | Continue from the cursor a previous page returned | - | No |
| `--all` | Follow cursors to list everything (at most 100 pages) | - | No |
| `--json` | Output in JSON format | - | No |

### `blob put` (alias: `upload`)

Upload a local file

**Usage:** `blob put <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--pathname <path>` | Where to store it (default: the file name) | - | No |
| `--content-type <type>` | Content type (default: guessed from the extension) | - | No |
| `--random-suffix` | Add a random suffix to the pathname so it never overwrites | - | No |
| `--json` | Output in JSON format | - | No |

### `blob get` (alias: `download`)

Download a blob to a file, or to stdout with -o -

**Usage:** `blob get <pathname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `-o, --output <path>` | Where to write it (default: its file name in this directory; - for stdout) | - | No |

### `blob head` (alias: `stat`)

Show a blob's metadata (size, content type, last modified) without downloading it

**Usage:** `blob head <pathname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `blob copy` (alias: `cp`)

Copy a blob (by pathname or URL) to a new pathname

**Usage:** `blob copy <from> <to> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `blob del` (alias: `delete`)

Delete one or more blobs

**Usage:** `blob del <pathnames...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `blob status`

Show whether the Blob service is enabled and healthy on this instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `blob enable`

Enable the Blob service on this instance (starts its S3-compatible container)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Docker image to run (default: the one this Temps release pins) | - | No |
| `--root-user <user>` | Root user for S3 access (default: generated) | - | No |
| `--root-password <password>` | Root password for S3 access (prefer --root-password-stdin) | - | No |
| `--json` | Output in JSON format | - | No |
| `--root-password-file <path>` | Read the root password from a file (- reads stdin) instead of --root-password | - | No |
| `--root-password-stdin` | Read the root password from stdin instead of --root-password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `blob update`

Change the Docker image the Blob service runs (recreates its container)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Docker image to run | - | Yes |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `blob disable`

Disable the Blob service on this instance (stops its container; every project loses Blob access)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `blob container-name`

Show, or set with --container-name/--reset, the Docker container name of the Blob service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--container-name <name>` | New container name (must keep the required prefix) | - | No |
| `--reset` | Go back to the default container name | - | No |
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
- `get-visible-by-hostname` - Find the project assignment for a certificate hostname
- `reassign` - Move a domain between projects without deleting its route or certificate

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
| `--service <name>` | Docker Compose service the domain routes to (compose projects only) | - | No |
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
| `--service <name>` | Docker Compose service the domain routes to ("" routes to the default service) | - | No |

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

### `custom-domains get-visible-by-hostname`

Find the project assignment for a certificate hostname

**Usage:** `custom-domains get-visible-by-hostname <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `custom-domains reassign`

Move a domain between projects without deleting its route or certificate

**Usage:** `custom-domains reassign <source-project-id> <domain-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--target-environment-id <n>` | Body field target_environment_id [required] | - | No |
| `--target-project-id <n>` | Body field target_project_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

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
- `update-managed-domain` - Update a managed domain's settings (hostname mode, sync opt-in, auto-manage)
- `apply-hostname-mode` - Apply a hostname mode to a managed domain (persist + optional DNS sync + route reload)
- `preview-hostname-mode` - Preview the impact of switching a managed domain's hostname mode

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
| `-t, --type <type>` | Provider type (cloudflare, bunny, route53, digitalocean, namecheap, gcp, azure, manual, pebble) | - | No |
| `-d, --description <description>` | Provider description | - | No |
| `--api-token <token>` | API token (Cloudflare, DigitalOcean; prefer --api-token-stdin to keep it out of shell history) | - | No |
| `--api-token-stdin` | Read the API token from stdin | - | No |
| `--account-id <id>` | Cloudflare account ID (optional) | - | No |
| `--access-key-id <key>` | AWS access key ID | - | No |
| `--secret-access-key <secret>` | AWS secret access key (prefer --secret-access-key-stdin to keep it out of shell history) | - | No |
| `--secret-access-key-stdin` | Read the AWS secret access key from stdin | - | No |
| `--region <region>` | AWS region | - | No |
| `--api-user <user>` | Namecheap API user | - | No |
| `--api-key <key>` | API key (Bunny, Namecheap; prefer --api-key-stdin to keep it out of shell history) | - | No |
| `--api-key-stdin` | Read the Bunny or Namecheap API key from stdin | - | No |
| `--username <username>` | Namecheap username | - | No |
| `--client-ip <ip>` | Namecheap whitelisted client IP | - | No |
| `--project-id <id>` | GCP project ID | - | No |
| `--service-account-email <email>` | GCP service account email | - | No |
| `--private-key-id <id>` | GCP private key ID | - | No |
| `--private-key <key>` | GCP private key (prefer --private-key-stdin to keep it out of shell history) | - | No |
| `--private-key-stdin` | Read the GCP private key from stdin | - | No |
| `--tenant-id <id>` | Azure tenant ID | - | No |
| `--client-id <id>` | Azure client ID | - | No |
| `--client-secret <secret>` | Azure client secret (prefer --client-secret-stdin to keep it out of shell history) | - | No |
| `--client-secret-stdin` | Read the Azure client secret from stdin | - | No |
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
| `--api-key <key>` | New API key/token (prefer --api-key-stdin to keep it out of shell history) | - | No |
| `--api-key-stdin` | Read the new API key/token from stdin | - | No |
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
| `--generated-hostname-mode <value>` | Layout of generated hostnames (one of: standard, flat) | - | No |
| `--proxied-by-default` | Create records proxied through the provider (Cloudflare) by default | - | No |
| `--no-proxied-by-default` | Set proxied_by_default to false | - | No |
| `--sync-generated-records` | Keep generated hostnames' DNS records in this zone in sync | - | No |
| `--no-sync-generated-records` | Set sync_generated_records to false | - | No |

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

### `dns-provider update-managed-domain`

Update a managed domain's settings (hostname mode, sync opt-in, auto-manage)

**Usage:** `dns-provider update-managed-domain <provider-id> <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--auto-manage` | Toggle automatic DNS management for this domain | - | No |
| `--no-auto-manage` | Set auto_manage to false | - | No |
| `--generated-hostname-mode <value>` | '"standard"' or '"flat"' | - | No |
| `--proxied-by-default` | Default proxy mode for newly managed records; 'false' is an explicit override | - | No |
| `--no-proxied-by-default` | Set proxied_by_default to false | - | No |
| `--sync-generated-records` | Toggle DNS record sync for this domain | - | No |
| `--no-sync-generated-records` | Set sync_generated_records to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `dns-provider apply-hostname-mode`

Apply a hostname mode to a managed domain (persist + optional DNS sync + route reload)

**Usage:** `dns-provider apply-hostname-mode <provider-id> <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--adopt-records <json>` | Conflicting records, from the preview's 'conflicts', that the user confirmed the sync may adopt — one... (JSON) | - | No |
| `--mode <value>` | Target mode to apply: '"standard"' or '"flat"' [required] | - | No |
| `--skip-records <json>` | Conflicting hostnames, from the preview's 'conflicts', that the user chose to leave untouched: the sync... (JSON) | - | No |
| `--sync-dns` | Also reconcile the provider's DNS zone for the affected hostnames | - | No |
| `--no-sync-dns` | Set sync_dns to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `dns-provider preview-hostname-mode`

Preview the impact of switching a managed domain's hostname mode

**Usage:** `dns-provider preview-hostname-mode <provider-id> <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | Target mode: standard\|flat [required] | - | No |
| `--sync` | Include DNS record changes | - | No |
| `--no-sync` | Set sync to false | - | No |
| `--json` | Output in JSON format | - | No |

## `delivery-profiles` (alias: `delivery-profile`)

Manage traffic delivery profiles (Cloudflare proxy, Bunny CDN) used by project domains

**Subcommands:**

- `capabilities` (`caps`) - Show which delivery providers are available and what each one still needs
- `list` (`ls`) - List delivery profiles, one page at a time (newest first)
- `create` (`add`) - Create a delivery profile
- `remove` (`rm`, `delete`) - Delete a delivery profile

### `delivery-profiles capabilities` (alias: `caps`)

Show which delivery providers are available and what each one still needs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `delivery-profiles list` (alias: `ls`)

List delivery profiles, one page at a time (newest first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Profiles per page, 1-100 (default: 20) | - | No |
| `--sort-by <field>` | Sort field: created_at, name (default: created_at) | - | No |
| `--sort-order <order>` | asc or desc (default: desc) | - | No |
| `--search <text>` | Only profiles whose name contains this text, ignoring case (at most 100 characters) | - | No |
| `--json` | Output the page as JSON (items, total, page, page_size) | - | No |

### `delivery-profiles create` (alias: `add`)

Create a delivery profile

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Profile name (1-100 characters) | - | No |
| `-k, --kind <kind>` | Delivery provider (cloudflare, bunny, direct) | - | No |
| `--pull-zone-id <id>` | Bunny Pull Zone ID (required for --kind bunny) | - | No |
| `--api-key <key>` | Bunny account API key (prefer --api-key-stdin to keep it out of shell history) | - | No |
| `--api-key-stdin` | Read the Bunny API key from stdin | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Never prompt (for automation) | - | No |

### `delivery-profiles remove` (alias: `rm`, `delete`)

Delete a delivery profile

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Profile ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

## `delivery`

Route project domains through a delivery provider (Cloudflare proxy, Bunny CDN)

**Subcommands:**

- `settings` - Project default delivery profile and per-environment overrides
- `bindings` (`binding`) - Domain delivery bindings: the DNS record + provider routing for a hostname

### `delivery settings`

Project default delivery profile and per-environment overrides

**Subcommands:**

- `get` (`show`) - Show the delivery profile a project and its environments use
- `set` - Change the project default profile and/or environment overrides

#### `delivery settings get` (alias: `show`)

Show the delivery profile a project and its environments use

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

#### `delivery settings set`

Change the project default profile and/or environment overrides

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--default-profile <id>` | Project default delivery profile ID, or "none" to clear it | - | No |
| `--env <environment-id=profile-id>` | Environment override, e.g. 12=3; use 12=inherit to fall back to the project default (repeatable) | `` | No |
| `--json` | Output in JSON format | - | No |

### `delivery bindings` (alias: `binding`)

Domain delivery bindings: the DNS record + provider routing for a hostname

**Subcommands:**

- `list` (`ls`) - List domain delivery bindings for a project, one page at a time (newest first)
- `preview` - Plan a delivery binding without changing DNS; prints a preview ID to apply
- `apply` - Apply a previewed delivery binding (writes DNS)
- `remove` (`rm`, `delete`) - Remove a delivery binding and the DNS record it manages

#### `delivery bindings list` (alias: `ls`)

List domain delivery bindings for a project, one page at a time (newest first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Bindings per page, 1-100 (default: 20) | - | No |
| `--sort-by <field>` | Sort field: created_at, hostname, updated_at (default: created_at) | - | No |
| `--sort-order <order>` | asc or desc (default: desc) | - | No |
| `--json` | Output the page as JSON (items, total, page, page_size) | - | No |

#### `delivery bindings preview`

Plan a delivery binding without changing DNS; prints a preview ID to apply

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Environment ID the hostname serves | - | No |
| `--hostname <hostname>` | Hostname to route, e.g. app.example.com | - | No |
| `--zone <zone>` | DNS zone that contains the hostname, e.g. example.com | - | No |
| `--dns-provider <id>` | DNS provider ID that manages the zone | - | No |
| `--origin-target <target>` | Origin the record points at (IP for A/AAAA, hostname for CNAME) | - | No |
| `--profile <id>` | Delivery profile ID (defaults to the environment or project profile) | - | No |
| `--json` | Output in JSON format | - | No |

#### `delivery bindings apply`

Apply a previewed delivery binding (writes DNS)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--preview-id <id>` | Preview ID returned by `delivery bindings preview` | - | Yes |
| `--adopt <TYPE:name>` | Adopt an existing unmanaged record the preview flagged, e.g. CNAME:www (repeatable) | `` | No |
| `--json` | Output in JSON format | - | No |

#### `delivery bindings remove` (alias: `rm`, `delete`)

Remove a delivery binding and the DNS record it manages

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--id <id>` | Binding ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

## `ip-access` (alias: `ipa`)

Manage instance-wide IP access control rules, and per-project lists (ip-access project)

**Subcommands:**

- `list` (`ls`) - List all IP access control rules
- `create` (`add`) - Create a new IP access control rule
- `show` - Show IP access control rule details
- `update` - Update an IP access control rule
- `remove` (`rm`) - Delete an IP access control rule
- `check` - Check if an IP address is blocked
- `project` - Allow or deny IP ranges for one project or environment
- `sessions` - Sessions commands

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
| `--confirm-broad-range` | Save a block range wider than /8 (IPv4) or /16 (IPv6) | - | No |
| `--acknowledge-lockout` | Save a block rule even if it matches the address you are calling from | - | No |
| `-y, --yes` | Skip prompts (for automation); does not confirm a broad range or a lockout | - | No |

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
| `--confirm-broad-range` | Save a block range wider than /8 (IPv4) or /16 (IPv6) | - | No |
| `--acknowledge-lockout` | Save a block rule even if it matches the address you are calling from | - | No |

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

### `ip-access project`

Allow or deny IP ranges for one project or environment

**Subcommands:**

- `show` - Show the IP access list
- `set` - Replace the IP access list. A list that would lock out this machine needs --acknowledge-lockout
- `clear` - Remove the list (an environment then follows the project list)
- `test` - Check whether an address would be allowed

#### `ip-access project show`

Show the IP access list

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID (omit for the project-wide list) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access project set`

Replace the IP access list. A list that would lock out this machine needs --acknowledge-lockout

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID (omit for the project-wide list) | - | No |
| `--mode <mode>` | disabled, allow (only listed) or deny (block listed) | - | Yes |
| `--cidr <cidr>` | Address or CIDR range; repeat or comma-separate | `` | No |
| `--tailscale` | Include the Tailscale ranges (100.64.0.0/10 and fd7a:115c:a1e0::/48) | - | No |
| `--acknowledge-lockout` | Apply even if it would block the address you are calling from | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access project clear`

Remove the list (an environment then follows the project list)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID (omit for the project-wide list) | - | No |
| `-y, --yes` | Skip confirmation | - | No |

#### `ip-access project test`

Check whether an address would be allowed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID (omit for the project-wide list) | - | No |
| `--ip <ip>` | Address to evaluate | - | Yes |
| `--json` | Output in JSON format | - | No |

### `ip-access sessions`

Sessions commands

**Subcommands:**

- `list-providers` - The saved Cloudflare Access tokens
- `create-provider` - Check a Cloudflare Access token, then save it sealed
- `check-provider` - Check which Cloudflare Access permissions a pasted token has: Access: Apps and Policies Edit (probed with..
- `delete-provider` - Delete a saved Cloudflare Access token
- `sign-out-of-apps` - Sign out of every signed-in address
- `sign-everyone-out-of-address` - Sign everyone out of a signed-in address

#### `ip-access sessions list-providers`

The saved Cloudflare Access tokens

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions create-provider`

Check a Cloudflare Access token, then save it sealed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--account-id <value>` | Body field account_id [required] | - | No |
| `--name <value>` | A name to tell tokens apart [required] | - | No |
| `--token-stdin` | Checked, then sealed; never returned [required] (read from stdin) | - | No |
| `--token-file <path>` | Checked, then sealed; never returned [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions check-provider`

Check which Cloudflare Access permissions a pasted token has: Access: Apps and Policies Edit (probed with..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--account-id <value>` | The Cloudflare account the Access applications live in (32 hex characters, from the dashboard URL or the... [required] | - | No |
| `--token-stdin` | The token, as pasted [required] (read from stdin) | - | No |
| `--token-file <path>` | The token, as pasted [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions delete-provider`

Delete a saved Cloudflare Access token

**Usage:** `ip-access sessions delete-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions sign-out-of-apps`

Sign out of every signed-in address

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions sign-everyone-out-of-address`

Sign everyone out of a signed-in address

**Usage:** `ip-access sessions sign-everyone-out-of-address <address-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

## `preview-gateway` (alias: `pgw`)

Manage the preview gateway that serves workspace preview URLs (status, settings, logs, restart, upgrade)

**Subcommands:**

- `status` - Show the gateway container's health, image and last setup error
- `settings` (`show`) - Show the gateway settings: enabled, image, host port, auto-upgrade, container name, and their defaults
- `configure` (`set`) - Change gateway settings; only the flags you pass are changed
- `logs` - Print the latest gateway container log lines
- `restart` - Restart the gateway container (previews are briefly unavailable)
- `upgrade` - Pull an image and recreate the gateway with it (previews are briefly unavailable)

### `preview-gateway status`

Show the gateway container's health, image and last setup error

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `preview-gateway settings` (alias: `show`)

Show the gateway settings: enabled, image, host port, auto-upgrade, container name, and their defaults

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `preview-gateway configure` (alias: `set`)

Change gateway settings; only the flags you pass are changed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enable` | Run the gateway (creates its containers) | - | No |
| `--disable` | Stop running the gateway (removes its containers; preview URLs stop being served) | - | No |
| `--image <ref>` | Gateway image to run, ideally an immutable @sha256 reference | - | No |
| `--reset-image` | Go back to the image pinned in this Temps build | - | No |
| `--host-port <port>` | Host port the ingress relay's :8080 is published on | - | No |
| `--auto-upgrade` | Apply the pinned image on every Temps startup (default) | - | No |
| `--no-auto-upgrade` | Leave the running image alone on startup; upgrade by hand | - | No |
| `--container-name <name>` | Docker container name; change only when several Temps instances share one Docker daemon | - | No |
| `--reset-container-name` | Go back to the default container name | - | No |
| `-y, --yes` | Do not ask before a change that interrupts previews (--disable, a new container name) | - | No |
| `--json` | Print the saved settings as JSON | - | No |

### `preview-gateway logs`

Print the latest gateway container log lines

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--tail <lines>` | Lines to print, 1 to 2000 (default 200) | - | No |
| `--json` | Output in JSON format | - | No |

### `preview-gateway restart`

Restart the gateway container (previews are briefly unavailable)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |

### `preview-gateway upgrade`

Pull an image and recreate the gateway with it (previews are briefly unavailable)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Image to pull and run, ideally an immutable @sha256 reference | - | No |
| `--default` | Use the image pinned in this Temps build | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |

## `edge-rules` (alias: `edge-rule`)

Block, allow, challenge, redirect or log requests before they reach the app. Rules run top to bottom; the first match decides

**Subcommands:**

- `list` (`ls`) - List the rules in evaluation order
- `get` - Show one rule
- `create` - Add a rule. New rules start in log-only mode unless --mode enforce is given
- `update` - Change a rule; only the given parts change
- `delete` (`rm`) - Delete a rule
- `move` - Move a rule to another position
- `enable` - Enable a rule
- `disable` - Disable a rule (kept, never evaluated)
- `enforce` - Act on matches
- `log-only` - Only record matches, never act on them
- `test` - Dry-run a request against the saved rules (or a rules file) and show the trace
- `export` - Print the rules as a file apply accepts
- `apply` - Make the project rules match a file: rules are matched by name, changed ones updated, missing ones created, others deleted, then ordered as in the file

### `edge-rules list` (alias: `ls`)

List the rules in evaluation order

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules get`

Show one rule

**Usage:** `edge-rules get <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules create`

Add a rule. New rules start in log-only mode unless --mode enforce is given

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--file <path>` | JSON file with the rule: {name?, expression, action, mode?, enabled?, environment_id?} | - | No |
| `--name <name>` | Rule name (generated from the rule when omitted) | - | No |
| `--expression <json>` | Match expression as JSON, e.g. {"combinator":"all","children":[...]} | - | No |
| `--action <json>` | Action as JSON, e.g. {"type":"block","status":403} | - | No |
| `--environment-id <id>` | Apply only to this environment | - | No |
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--mode <mode>` | enforce or log-only (default: the file's mode, else log-only) | - | No |
| `--disabled` | Create it disabled | - | No |
| `--position <n>` | Insert at this 1-based position (default: last) | - | No |

### `edge-rules update`

Change a rule; only the given parts change

**Usage:** `edge-rules update <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--file <path>` | JSON file with the rule: {name?, expression, action, mode?, enabled?, environment_id?} | - | No |
| `--name <name>` | Rule name (generated from the rule when omitted) | - | No |
| `--expression <json>` | Match expression as JSON, e.g. {"combinator":"all","children":[...]} | - | No |
| `--action <json>` | Action as JSON, e.g. {"type":"block","status":403} | - | No |
| `--environment-id <id>` | Apply only to this environment | - | No |
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--mode <mode>` | enforce or log-only | - | No |
| `--all-environments` | Apply to every environment | - | No |
| `--revision <n>` | Fail if the rule changed since this revision (default: the current one) | - | No |

### `edge-rules delete` (alias: `rm`)

Delete a rule

**Usage:** `edge-rules delete <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation | - | No |

### `edge-rules move`

Move a rule to another position

**Usage:** `edge-rules move <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--to <n>` | New 1-based position | - | Yes |

### `edge-rules enable`

Enable a rule

**Usage:** `edge-rules enable <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules disable`

Disable a rule (kept, never evaluated)

**Usage:** `edge-rules disable <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules enforce`

Act on matches

**Usage:** `edge-rules enforce <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules log-only`

Only record matches, never act on them

**Usage:** `edge-rules log-only <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `edge-rules test`

Dry-run a request against the saved rules (or a rules file) and show the trace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--url <url>` | Request URL or path, e.g. https://app.example.com/admin?x=1 | - | Yes |
| `--method <method>` | HTTP method | `GET` | No |
| `--header <header>` | "Name: value"; repeatable | `` | No |
| `--ip <ip>` | Client address | - | No |
| `--country <code>` | ISO country code, e.g. DE | - | No |
| `--continent <code>` | Continent code, e.g. EU | - | No |
| `--asn <n>` | Autonomous system number | - | No |
| `--user-agent <ua>` | User agent | - | No |
| `--bot-class <class>` | human, search_engine, ai_crawler, ai_agent or unknown_bot | - | No |
| `--environment-id <id>` | Environment the request is for | - | No |
| `--cleared` | The visitor already solved a challenge | - | No |
| `--file <path>` | Evaluate these rules instead of the saved ones (same format as export) | - | No |

### `edge-rules export`

Print the rules as a file apply accepts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--output <path>` | Write to a file instead of stdout | - | No |

### `edge-rules apply`

Make the project rules match a file: rules are matched by name, changed ones updated, missing ones created, others deleted, then ordered as in the file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--file <path>` | Rules file ({"rules": [...]}, as written by export) | - | Yes |
| `--dry-run` | Show the plan without changing anything | - | No |
| `-y, --yes` | Skip confirmation | - | No |

## `network-policy` (alias: `netpol`)

Manage network policy for workloads: rules, egress/ingress profiles and the effective policy

**Subcommands:**

- `rules` - Manage network policy rules
- `effective` - Show the policy in force for a workload: profile, rules, inbound links and where it is enforced. <kind> is one of deployment, sandbox, service, function, job; <id> is the environment id for a deployment
- `profile` - Manage a workload's egress and ingress profile
- `list-node-modes` - List node modes
- `set-node-mode` - Set node mode

### `network-policy rules`

Manage network policy rules

**Subcommands:**

- `list` (`ls`) - List network policy rules
- `show` - Show one network policy rule
- `create` (`add`) - Create a network policy rule
- `rm` (`remove`) - Delete a network policy rule (link rules are removed by unlinking instead)

#### `network-policy rules list` (alias: `ls`)

List network policy rules

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--subject-kind <kind>` | Only rules for this kind (deployment, sandbox, service, function, job) | - | No |
| `--subject-id <id>` | Only rules for this subject (environment id for deployments) | - | No |
| `--origin <origin>` | Only rules of this origin (link, operator, system) | - | No |
| `--page <n>` | Page number (default 1) | - | No |
| `--page-size <n>` | Rules per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `network-policy rules show`

Show one network policy rule

**Usage:** `network-policy rules show <ruleId> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `network-policy rules create` (alias: `add`)

Create a network policy rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--subject-kind <kind>` | Workload kind (deployment, sandbox, service, function, job) | - | Yes |
| `--subject-id <id>` | Environment id for a deployment; the row id for other kinds | - | Yes |
| `--direction <direction>` | Traffic direction (egress, ingress) | - | Yes |
| `--peer-kind <kind>` | Peer type: service (by id), workload (<kind>/<id>) or cidr. host rules are refused until a node runs the shared egress proxy | - | Yes |
| `--peer <ref>` | Peer reference: a service id, <kind>/<id>, or a CIDR | - | Yes |
| `--action <action>` | What to do with matching traffic (allow, deny) | - | Yes |
| `--protocol <protocol>` | tcp, udp, any (default tcp) | - | No |
| `--port <port>` | Port, or the start of a range. Omit for every port | - | No |
| `--port-end <port>` | End of the port range (needs --port) | - | No |
| `--description <text>` | Why this rule exists | - | No |
| `--json` | Output the created rule in JSON format | - | No |

#### `network-policy rules rm` (alias: `remove`)

Delete a network policy rule (link rules are removed by unlinking instead)

**Usage:** `network-policy rules rm <ruleId> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `network-policy effective`

Show the policy in force for a workload: profile, rules, inbound links and where it is enforced. <kind> is one of deployment, sandbox, service, function, job; <id> is the environment id for a deployment

**Usage:** `network-policy effective <kind> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `network-policy profile`

Manage a workload's egress and ingress profile

**Subcommands:**

- `set` - Set a workload's profile. Each direction is open, restricted, none; a direction left out is reset to the kind's default

#### `network-policy profile set`

Set a workload's profile. Each direction is open, restricted, none; a direction left out is reset to the kind's default

**Usage:** `network-policy profile set <kind> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--egress <profile>` | Outbound profile (open, restricted, none). Omit for the kind's default | - | No |
| `--ingress <profile>` | Inbound profile (open, restricted, none). Omit for the kind's default | - | No |
| `--json` | Output in JSON format | - | No |

### `network-policy list-node-modes`

List node modes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `network-policy set-node-mode`

Set node mode

**Usage:** `network-policy set-node-mode <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | Body field mode [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
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
| `--resource-id <id>` | Filter by the ID of the resource acted on | - | No |
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
- `get-api-traffic-access` - Report whether the current principal may drill into a project's proxy logs
- `list-known-ai-agents` - List every AI agent the detector knows how to classify
- `get-ai-agent-pages` - Get the top pages accessed by a specific AI agent over a time window
- `get-ai-agent-timeline` - Time-bucketed AI-agent request volume, split by provider or agent
- `get-ai-status-breakdown` - HTTP status-class breakdown for AI-agent traffic — are bots being served (2xx) or hitting broken/blocked..
- `get-health` - Get user-traffic health summaries for multiple projects (last 1 hour)

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
| `--path-exact <value>` | Only this exact path (--path matches a prefix) | - | No |
| `--session-id <n>` | Only requests of this analytics session | - | No |
| `--visitor-id <n>` | Only requests of this visitor | - | No |
| `--exclude-bots` | Leave out requests from bots | - | No |
| `--no-exclude-bots` | Set exclude_bots to false | - | No |
| `--exclude-synthetic` | Leave out synthetic traffic (uptime checks, Temps itself) | - | No |
| `--no-exclude-synthetic` | Set exclude_synthetic to false | - | No |
| `--is-ai-agent` | Only (or, with --no-is-ai-agent, never) AI agent requests | - | No |
| `--no-is-ai-agent` | Set is_ai_agent to false | - | No |
| `--ai-agent <value>` | Only this AI agent (e.g. GPTBot) | - | No |
| `--ai-provider <value>` | Only AI agents of this provider (e.g. OpenAI) | - | No |
| `--request-size-min <n>` | Smallest request body, in bytes | - | No |
| `--request-size-max <n>` | Largest request body, in bytes | - | No |
| `--response-size-min <n>` | Smallest response body, in bytes | - | No |
| `--response-size-max <n>` | Largest response body, in bytes | - | No |
| `--response-time-min <n>` | Fastest response, in milliseconds | - | No |
| `--response-time-max <n>` | Slowest response, in milliseconds | - | No |

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
| `--has-project` | Only requests routed to a project (or, with --no-has-project, to none) | - | No |
| `--no-has-project` | Set has_project to false | - | No |
| `--status-code-class <value>` | Only this status class (one of: 2xx, 3xx, 4xx, 5xx) | - | No |

### `proxy-logs today`

Get today's request statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--status-code-class <value>` | Only this status class (one of: 2xx, 3xx, 4xx, 5xx) | - | No |

### `proxy-logs get-api-traffic-access`

Report whether the current principal may drill into a project's proxy logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs list-known-ai-agents`

List every AI agent the detector knows how to classify

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `proxy-logs get-ai-agent-pages`

Get the top pages accessed by a specific AI agent over a time window

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--agent <value>` | Canonical agent name to filter by (e.g. 'ChatGPT-User', 'ClaudeBot') [required] | - | No |
| `--project-id <n>` | Filter by project ID | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--start-time <value>` | Start time (ISO 8601) | - | No |
| `--end-time <value>` | End time (ISO 8601) | - | No |
| `--limit <n>` | Maximum rows to return | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs get-ai-agent-timeline`

Time-bucketed AI-agent request volume, split by provider or agent

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter by project ID | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--start-time <value>` | Start time (ISO 8601) | - | No |
| `--end-time <value>` | End time (ISO 8601) | - | No |
| `--group-by <value>` | Grouping dimension: 'provider' (default) or 'agent' | - | No |
| `--bucket <value>` | Bucket interval override (e.g. '1 hour', '1 day') | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs get-ai-status-breakdown`

HTTP status-class breakdown for AI-agent traffic — are bots being served (2xx) or hitting broken/blocked..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter by project ID (recommended for per-project analytics) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--start-time <value>` | Start time (ISO 8601) | - | No |
| `--end-time <value>` | End time (ISO 8601) | - | No |
| `--limit <n>` | Maximum rows to return | - | No |
| `--path <value>` | Optional exact path filter | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs get-health`

Get user-traffic health summaries for multiple projects (last 1 hour)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-ids <value>` | Comma-separated list of project IDs [required] | - | No |
| `--start-time <value>` | Optional start time (ISO 8601) | - | No |
| `--end-time <value>` | Optional end time (ISO 8601) | - | No |
| `--is-bot` | Filter by bot detection | - | No |
| `--no-is-bot` | Set is_bot to false | - | No |
| `--json` | Output in JSON format | - | No |

## `logs` (alias: `glogs`)

Search collected logs across every project and database you can access

**Subcommands:**

- `search` - Search log lines across projects (newest first)
- `facets` - Distinct values and counts per field for the same filter scope
- `attributes` - Attribute keys observed in the window, most common first (requires the ClickHouse line index)
- `histogram` - Line counts bucketed over time, optionally split by a label or attribute (requires the ClickHouse line index)
- `aggregate` - Group-by aggregation over lines (requires the ClickHouse line index)
- `capabilities` - Whether container logs are being collected, and whether attribute facets, histograms and aggregates are available
- `saved` - Saved log searches: named filters you can reopen, chart on a dashboard and alert on
- `keys` - Ingest keys for POST /api/logs/ingest (newline-delimited JSON from Vector, Fluent Bit or any HTTP client)
- `get-context` - Get context lines surrounding a specific log line
- `global-aggregate` - Global log aggregate
- `global-attribute-keys` - Global log attribute keys
- `global-capabilities` - Global log capabilities
- `global-facets-attrs` - Global log facets attrs
- `global-histogram` - Global log histogram
- `ingest` - Ingest newline-delimited JSON log lines with a log ingest key
- `ingest-budget` - The ingestion budget and the containers dropping lines over it, for the warnings on the log views
- `tail` - Live tail logs via Server-Sent Events
- `purge` - Purge all logs for a project before a given timestamp
- `project-search` - Search one project's (or one database's) log lines, newest first, with a cursor for the next page
- `get-saved-search` - Read one saved log search
- `update-saved-search` - Change a saved log search

### `logs search`

Search log lines across projects (newest first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Lines per request (default: 200, max: 1000) | - | No |
| `--cursor <token>` | Resume from a previous next_cursor | - | No |
| `--all` | Follow next_cursor automatically until the results run out (or --max-pages is reached, default 20 pages) | - | No |
| `--max-pages <n>` | Page ceiling for --all (default: 20) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id\|slug\|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id\|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs facets`

Distinct values and counts per field for the same filter scope

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--field <name>` | Facet field, repeatable: env, service, level, stream, project, external_service, node, deploy, container (default: env, service, level, node, deploy) | `` | No |
| `--attr-keys <keys>` | Comma-separated label names and/or attr:<name> (e.g. worker,attr:cache) — routes to the attribute-aware endpoint, which requires the ClickHouse line index | - | No |
| `--limit <n>` | Max values per key when using --attr-keys or --attr (default 50, cap 1000) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id\|slug\|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id\|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs attributes`

Attribute keys observed in the window, most common first (requires the ClickHouse line index)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max keys returned (default 100, cap 1000) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id\|slug\|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id\|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs histogram`

Line counts bucketed over time, optionally split by a label or attribute (requires the ClickHouse line index)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--bucket-secs <n>` | Bucket width in seconds (default 60) | - | No |
| `--group-by <key>` | One label name or attr:<name> to split series by | - | No |
| `--max-groups <n>` | Max series before folding the rest into "other" (default 8) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id\|slug\|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id\|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs aggregate`

Group-by aggregation over lines (requires the ClickHouse line index)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--group-by <keys>` | Comma-separated label names and/or attr:<name> | - | Yes |
| `--metric <spec>` | count \| count_distinct:<k> \| avg:<k> \| p50:<k> \| p95:<k> \| p99:<k> \| max:<k> \| sum:<k> | - | Yes |
| `--limit <n>` | Max rows returned (default 50, cap 1000) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id\|slug\|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id\|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs capabilities`

Whether container logs are being collected, and whether attribute facets, histograms and aggregates are available

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `logs saved`

Saved log searches: named filters you can reopen, chart on a dashboard and alert on

**Subcommands:**

- `list` - List the saved searches of a project, pinned first
- `create` - Save a log search
- `delete` - Delete a saved search (refused while an alert rule counts it)
- `series` - Chart a saved search over time: matching lines per bucket, or a numeric field

#### `logs saved list`

List the saved searches of a project, pinned first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <slug\|id>` | Project (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `logs saved create`

Save a log search

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Name, unique within the project | - | Yes |
| `--project <slug\|id>` | Project (defaults to the linked project) | - | No |
| `--description <text>` | What the search is for | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--level <level>` | Log level, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Service label, repeatable | `` | No |
| `--container <id>` | Container id, repeatable | `` | No |
| `--node <id>` | Node id, repeatable | `` | No |
| `--deploy <id>` | Deployment id | - | No |
| `--text <needle>` | Text the message must contain | - | No |
| `--attr <predicate>` | Attribute predicate, repeatable: key=value, key!=value, key^=prefix, key>n, key<n, key? | `` | No |
| `--time-range <range>` | Range the search opens with: 15m, 1h, 6h, 24h, 7d, 30d (default 1h) | - | No |
| `--pinned` | Pin it to the top of the list | - | No |
| `--json` | Output in JSON format | - | No |

#### `logs saved delete`

Delete a saved search (refused while an alert rule counts it)

**Usage:** `logs saved delete <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <slug\|id>` | Project (defaults to the linked project) | - | No |

#### `logs saved series`

Chart a saved search over time: matching lines per bucket, or a numeric field

**Usage:** `logs saved series <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <slug\|id>` | Project (defaults to the linked project) | - | No |
| `--since <duration>` | Window ending now, e.g. 30m, 6h, 1d (default 1h) | - | No |
| `--bucket-secs <n>` | Bucket width in seconds (default 60) | - | No |
| `--metric <spec>` | count (default), or avg\|sum\|max\|p50\|p95\|p99\|count_distinct:<attribute> | - | No |
| `--json` | Output in JSON format | - | No |

### `logs keys`

Ingest keys for POST /api/logs/ingest (newline-delimited JSON from Vector, Fluent Bit or any HTTP client)

**Subcommands:**

- `list` - List the ingest keys of a project
- `create` - Create an ingest key; the key is printed once
- `revoke` - Revoke an ingest key; collectors using it are refused

#### `logs keys list`

List the ingest keys of a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <slug\|id>` | Project (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `logs keys create`

Create an ingest key; the key is printed once

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Name, e.g. the host or collector | - | Yes |
| `--project <slug\|id>` | Project (defaults to the linked project) | - | No |
| `--service <name>` | Service label for lines that name none | - | No |
| `--environment <id>` | Pin lines to an environment id | - | No |
| `--json` | Output in JSON format | - | No |

#### `logs keys revoke`

Revoke an ingest key; collectors using it are refused

**Usage:** `logs keys revoke <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <slug\|id>` | Project (defaults to the linked project) | - | No |

### `logs get-context`

Get context lines surrounding a specific log line

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--timestamp <value>` | Target line timestamp (RFC 3339) [required] | - | No |
| `--container-id <value>` | Target line container ID [required] | - | No |
| `--line-id <value>` | Target line_id, as a decimal string [required] | - | No |
| `--lines <n>` | Context lines before and after (default: 25, max 50) | - | No |
| `--json` | Output in JSON format | - | No |

### `logs global-aggregate`

Global log aggregate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-time <value>` | Window start (RFC 3339) [required] | - | No |
| `--end-time <value>` | Window end (RFC 3339) [required] | - | No |
| `--source <value>` | Source kind (default: all) (one of: collected, application, service) | - | No |
| `--projects <value>` | Project selectors (repeatable) (repeatable) | - | No |
| `--external-services <value>` | External service selectors (repeatable) (repeatable) | - | No |
| `--scopes <value>` | Scope selectors (repeatable) (repeatable) | - | No |
| `--levels <value>` | Levels (repeatable) (one of: TRACE, DEBUG, INFO, WARN, ERROR) (repeatable) | - | No |
| `--envs <value>` | Environments (repeatable) (repeatable) | - | No |
| `--services <value>` | Services (repeatable) (repeatable) | - | No |
| `--container-ids <value>` | Container ids (repeatable) (repeatable) | - | No |
| `--node-ids <n>` | Node ids (repeatable) (repeatable) | - | No |
| `--deploy-id <n>` | Deployment id | - | No |
| `--group-by <value>` | Comma-separated label names and/or attr:<name> [required] | - | No |
| `--metric <value>` | count \| count_distinct:<k> \| avg:<k> \| p50:<k> \| p95:<k> \| p99:<k> \| max:<k> \| sum:<k> [required] | - | No |
| `--limit <n>` | Max rows returned (default 50, cap 1000) | - | No |
| `--attr <value>` | Repeatable attribute predicate: <key><op><value> or <key>? (repeatable) [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `logs global-attribute-keys`

Global log attribute keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-time <value>` | Window start (RFC 3339) [required] | - | No |
| `--end-time <value>` | Window end (RFC 3339) [required] | - | No |
| `--source <value>` | Source kind (default: all) (one of: collected, application, service) | - | No |
| `--projects <value>` | Project selectors (repeatable) (repeatable) | - | No |
| `--external-services <value>` | External service selectors (repeatable) (repeatable) | - | No |
| `--scopes <value>` | Scope selectors (repeatable) (repeatable) | - | No |
| `--levels <value>` | Levels (repeatable) (one of: TRACE, DEBUG, INFO, WARN, ERROR) (repeatable) | - | No |
| `--envs <value>` | Environments (repeatable) (repeatable) | - | No |
| `--services <value>` | Services (repeatable) (repeatable) | - | No |
| `--container-ids <value>` | Container ids (repeatable) (repeatable) | - | No |
| `--node-ids <n>` | Node ids (repeatable) (repeatable) | - | No |
| `--deploy-id <n>` | Deployment id | - | No |
| `--limit <n>` | Max keys returned (default/cap 1000) | - | No |
| `--json` | Output in JSON format | - | No |

### `logs global-capabilities`

Global log capabilities

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `logs global-facets-attrs`

Global log facets attrs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-time <value>` | Window start (RFC 3339) [required] | - | No |
| `--end-time <value>` | Window end (RFC 3339) [required] | - | No |
| `--source <value>` | Source kind (default: all) (one of: collected, application, service) | - | No |
| `--projects <value>` | Project selectors (repeatable) (repeatable) | - | No |
| `--external-services <value>` | External service selectors (repeatable) (repeatable) | - | No |
| `--scopes <value>` | Scope selectors (repeatable) (repeatable) | - | No |
| `--levels <value>` | Levels (repeatable) (one of: TRACE, DEBUG, INFO, WARN, ERROR) (repeatable) | - | No |
| `--envs <value>` | Environments (repeatable) (repeatable) | - | No |
| `--services <value>` | Services (repeatable) (repeatable) | - | No |
| `--container-ids <value>` | Container ids (repeatable) (repeatable) | - | No |
| `--node-ids <n>` | Node ids (repeatable) (repeatable) | - | No |
| `--deploy-id <n>` | Deployment id | - | No |
| `--keys <value>` | Comma-separated label names or attr:<name> [required] | - | No |
| `--attr <value>` | Repeatable attribute predicate: <key><op><value> or <key>? (repeatable) [required] | - | No |
| `--limit <n>` | Max values per key (default/cap 1000) | - | No |
| `--json` | Output in JSON format | - | No |

### `logs global-histogram`

Global log histogram

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-time <value>` | Window start (RFC 3339) [required] | - | No |
| `--end-time <value>` | Window end (RFC 3339) [required] | - | No |
| `--source <value>` | Source kind (default: all) (one of: collected, application, service) | - | No |
| `--projects <value>` | Project selectors (repeatable) (repeatable) | - | No |
| `--external-services <value>` | External service selectors (repeatable) (repeatable) | - | No |
| `--scopes <value>` | Scope selectors (repeatable) (repeatable) | - | No |
| `--levels <value>` | Levels (repeatable) (one of: TRACE, DEBUG, INFO, WARN, ERROR) (repeatable) | - | No |
| `--envs <value>` | Environments (repeatable) (repeatable) | - | No |
| `--services <value>` | Services (repeatable) (repeatable) | - | No |
| `--container-ids <value>` | Container ids (repeatable) (repeatable) | - | No |
| `--node-ids <n>` | Node ids (repeatable) (repeatable) | - | No |
| `--deploy-id <n>` | Deployment id | - | No |
| `--bucket-secs <n>` | Bucket width in seconds (default 60) | - | No |
| `--group-by <value>` | One label name or attr:<name> to split series by | - | No |
| `--max-groups <n>` | Max series before folding the rest into "other" (default 8) | - | No |
| `--attr <value>` | Repeatable attribute predicate: <key><op><value> or <key>? (repeatable) [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `logs ingest`

Ingest newline-delimited JSON log lines with a log ingest key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--input <path>` | The file to upload (- for stdin) | - | Yes |
| `--json` | Output in JSON format | - | No |

### `logs ingest-budget`

The ingestion budget and the containers dropping lines over it, for the warnings on the log views

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `logs tail`

Live tail logs via Server-Sent Events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <value>` | Project ID [required] | - | No |
| `--service <value>` | Service name [required] | - | No |
| `--env <value>` | Environment [required] | - | No |
| `--levels <value>` | Optional level filters (repeatable) [required] | - | No |
| `--text <value>` | Optional text filter | - | No |
| `--json` | Output in JSON format | - | No |

### `logs purge`

Purge all logs for a project before a given timestamp

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--before <value>` | Delete all logs before this timestamp (ISO 8601) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `logs project-search`

Search one project's (or one database's) log lines, newest first, with a cursor for the next page

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID (0 with --external-service-id searches a database's logs) [required] | - | No |
| `--external-service-id <n>` | Search this external service's (database's) logs instead | - | No |
| `--start-time <value>` | Window start (RFC 3339; default: 24 hours ago) | - | No |
| `--end-time <value>` | Window end (RFC 3339; default: now) | - | No |
| `--text <value>` | Only lines containing this text | - | No |
| `--levels <value>` | Only these levels (repeatable) (one of: TRACE, DEBUG, INFO, WARN, ERROR) (repeatable) | - | No |
| `--envs <value>` | Only these environments (repeatable) (repeatable) | - | No |
| `--services <value>` | Only these services (repeatable) (repeatable) | - | No |
| `--container-ids <value>` | Only these containers (repeatable) (repeatable) | - | No |
| `--node-ids <n>` | Only lines from these nodes (repeatable) (repeatable) | - | No |
| `--deploy-id <n>` | Only lines from this deployment | - | No |
| `--context-lines <n>` | Lines of context around each match | - | No |
| `--page-size <n>` | Lines per page | - | No |
| `--cursor <value>` | Resume from a previous next_cursor | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `logs get-saved-search`

Read one saved log search

**Usage:** `logs get-saved-search <saved-search-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `logs update-saved-search`

Change a saved log search

**Usage:** `logs update-saved-search <saved-search-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--description <value>` | Absent leaves the description unchanged; an empty string clears it | - | No |
| `--name <value>` | Absent leaves the name unchanged | - | No |
| `--pinned` | Body field pinned | - | No |
| `--no-pinned` | Set pinned to false | - | No |
| `--query <json>` | The stored filter set of a saved search (JSON) | - | No |
| `--time-range <value>` | Body field time_range | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

## `email-domains` (alias: `edom`)

Manage email domains for transactional email

**Subcommands:**

- `list` (`ls`) - List all email domains
- `create` (`add`) - Create a new email domain
- `import` - Import an existing domain identity that was already provisioned in the provider console. Fetches the current verification state without re-creating the identity.
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

### `email-domains import`

Import an existing domain identity that was already provisioned in the provider console. Fetches the current verification state without re-creating the identity.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name (e.g., mail.example.com) | - | No |
| `--provider-id <id>` | Email provider ID | - | No |
| `--provider-identity-id <id>` | Provider-internal identity UUID (required for Scaleway; omit for SES) | - | No |
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
| `--delete-from-provider` | Also delete the domain at the email provider (SES, etc.) | - | No |
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
- `discoverable-domains` (`discover-domains`) - List domain identities already registered on the provider's side, for importing
- `update` - Update an email provider
- `setup-tracking` - One-click AWS-side setup of SES event tracking (SNS topic + webhook subscription + SESv2 event..
- `get-tracking-status` - Live status of SES event tracking for a provider

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
| `-t, --type <type>` | Provider type (ses, scaleway, smtp) | - | No |
| `-r, --region <region>` | Cloud region | - | No |
| `--access-key-id <key>` | AWS access key ID (for SES) | - | No |
| `--secret-access-key <secret>` | AWS secret access key (for SES) | - | No |
| `--api-key <key>` | Scaleway API key | - | No |
| `--project-id <id>` | Scaleway project ID | - | No |
| `--sns-topic-arn <arn>` | SES: the only SNS topic allowed to deliver bounce and complaint events | - | No |
| `--smtp-host <host>` | SMTP server host name | - | No |
| `--smtp-port <port>` | SMTP port (default: 587, or 465 with --smtp-encryption tls) | - | No |
| `--smtp-encryption <mode>` | starttls (default), tls or none | - | No |
| `--smtp-username <user>` | SMTP user name, when the server requires a sign-in | - | No |
| `--smtp-password <password>` | SMTP password | - | No |
| `--smtp-accept-invalid-certs` | Accept a self-signed or otherwise invalid server certificate | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--secret-access-key-file <path>` | Read the AWS secret access key from a file (- reads stdin) instead of --secret-access-key | - | No |
| `--secret-access-key-stdin` | Read the AWS secret access key from stdin instead of --secret-access-key | - | No |
| `--api-key-file <path>` | Read the Scaleway API key from a file (- reads stdin) instead of --api-key | - | No |
| `--api-key-stdin` | Read the Scaleway API key from stdin instead of --api-key | - | No |
| `--smtp-password-file <path>` | Read the SMTP password from a file (- reads stdin) instead of --smtp-password | - | No |
| `--smtp-password-stdin` | Read the SMTP password from stdin instead of --smtp-password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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

### `email-providers discoverable-domains` (alias: `discover-domains`)

List domain identities already registered on the provider's side, for importing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-providers update`

Update an email provider

**Usage:** `email-providers update <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--is-active` | Body field is_active | - | No |
| `--no-is-active` | Set is_active to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--region <value>` | Body field region | - | No |
| `--scaleway-credentials <json>` | Body field scaleway_credentials (JSON) | - | No |
| `--ses-credentials <json>` | Body field ses_credentials (JSON) | - | No |
| `--smtp-credentials <json>` | Generic SMTP credentials request body (JSON) | - | No |
| `--sns-topic-arn <value>` | Rotate or clear the exact SNS topic allowed for this SES provider | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `email-providers setup-tracking`

One-click AWS-side setup of SES event tracking (SNS topic + webhook subscription + SESv2 event..

**Usage:** `email-providers setup-tracking <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `email-providers get-tracking-status`

Live status of SES event tracking for a provider

**Usage:** `email-providers get-tracking-status <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `incidents` (alias: `incident`)

Manage incidents for status pages and monitoring

**Subcommands:**

- `list` (`ls`) - List incidents for a project
- `create` (`add`) - Create a new incident
- `show` - Show incident details
- `update-status` - Update an incident status
- `set-visibility` - Show or hide an incident update on published status pages
- `updates` - List status updates for an incident
- `bucketed` - Get bucketed incident data for a project
- `update` - Set or clear the heading an incident shows on published status pages

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
| `--public-title <title>` | Headline shown on published status pages (the title stays internal) | - | No |
| `-m, --message <message>` | First update, written by you instead of the automatic one | - | No |
| `--public` | Post the first update to published status pages (needs --message) | - | No |
| `--monitor-id <id>` | Monitor (status page component) the incident affects; omit for the whole project | - | No |
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
| `--public` | Show this update on published status pages (private by default) | - | No |

### `incidents set-visibility`

Show or hide an incident update on published status pages

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `--update-id <id>` | Incident update ID (see `incidents updates`) | - | Yes |
| `--public` | Show the update on published status pages | - | No |
| `--private` | Hide the update from published status pages | - | No |

### `incidents updates`

List status updates for an incident

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
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

### `incidents update`

Set or clear the heading an incident shows on published status pages

**Usage:** `incidents update <incident-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--public-title <value>` | New public heading [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

## `status-pages` (alias: `status-page`)

Publish public status pages showing monitors from any of your projects

**Subcommands:**

- `list` (`ls`) - List status pages, optionally only those showing one project
- `create` - Create a draft status page; monitors may come from any project
- `show` - Show a status page
- `update` - Change the title, description or monitor selection
- `publish` - Publish the page at its public URL
- `unpublish` - Unpublish; the public URL returns 404 immediately
- `rotate-url` - Replace the public URL; the old one stops working
- `delete` (`rm`) - Delete a status page; its public URL returns 404 immediately
- `domains` (`domain`) - Serve a status page on your own hostname
- `subscriptions` - Let visitors subscribe to a status page by email
- `subscribers` - List, export or delete the subscribers of a status page
- `maintenance` - Schedule maintenance windows: shown on the page, no outage alerts, no downtime counted
- `webhooks` - Post every public update of a status page to signed HTTPS webhooks
- `password` - Protect a status page with a password
- `get-overview` - Get status page overview
- `get-maintenance` - A maintenance window
- `preview` - Preview what a status page shows visitors, published or not
- `get-public-feed` - Public status page (Atom feed)
- `get-public-json` - Public status page (JSON)

### `status-pages list` (alias: `ls`)

List status pages, optionally only those showing one project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Only pages with a component from this project | - | No |
| `--json` | Output in JSON format | - | No |

### `status-pages create`

Create a draft status page; monitors may come from any project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --title <title>` | Public page title | - | Yes |
| `-m, --monitor <id=name>` | Monitor to show and its public name (repeatable) | - | Yes |
| `-g, --group <name=ids>` | Show monitors together under a group, e.g. "Cloud=12,13" (repeatable) | - | No |
| `-d, --description <text>` | Public description | - | No |
| `--json` | Output in JSON format | - | No |
| `--theme <value>` | Color scheme of the public page (one of: auto, light, dark) | - | No |
| `--accent-color <value>` | Accent color, e.g. #2563eb | - | No |
| `--logo-url <value>` | HTTPS URL of the logo shown in the header | - | No |
| `--website-url <value>` | Where the logo links to | - | No |
| `--show-response-times` | Show monitor response times | - | No |
| `--no-show-response-times` | Set show_response_times to false | - | No |
| `--show-uptime-percentage` | Show uptime percentages | - | No |
| `--no-show-uptime-percentage` | Set show_uptime_percentage to false | - | No |

### `status-pages show`

Show a status page

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `status-pages update`

Change the title, description or monitor selection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `-t, --title <title>` | Public page title | - | No |
| `-d, --description <text>` | Public description ("" clears it) | - | No |
| `-m, --monitor <id=name>` | Replace the selection; repeat for each monitor | - | No |
| `-g, --group <name=ids>` | With --monitor: group monitors, e.g. "Cloud=12,13" (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |
| `--theme <value>` | Color scheme of the public page (one of: auto, light, dark) | - | No |
| `--accent-color <value>` | Accent color, e.g. #2563eb | - | No |
| `--logo-url <value>` | HTTPS URL of the logo shown in the header | - | No |
| `--website-url <value>` | Where the logo links to | - | No |
| `--show-response-times` | Show monitor response times | - | No |
| `--no-show-response-times` | Set show_response_times to false | - | No |
| `--show-uptime-percentage` | Show uptime percentages | - | No |
| `--no-show-uptime-percentage` | Set show_uptime_percentage to false | - | No |

### `status-pages publish`

Publish the page at its public URL

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `status-pages unpublish`

Unpublish; the public URL returns 404 immediately

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `status-pages rotate-url`

Replace the public URL; the old one stops working

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `status-pages delete` (alias: `rm`)

Delete a status page; its public URL returns 404 immediately

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages domains` (alias: `domain`)

Serve a status page on your own hostname

**Subcommands:**

- `list` (`ls`) - List a status page's custom domains and the DNS records they need
- `add` - Attach a hostname; it serves only after ownership is verified
- `verify` - Prove ownership and issue the certificate
- `renew` - Renew the certificate; the current one keeps serving meanwhile (DNS-01: prints the new TXT record)
- `remove` (`rm`) - Detach a hostname; it answers 404 immediately and its certificate is deleted

#### `status-pages domains list` (alias: `ls`)

List a status page's custom domains and the DNS records they need

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages domains add`

Attach a hostname; it serves only after ownership is verified

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--hostname <hostname>` | Hostname, e.g. status.example.com | - | Yes |
| `--dns` | Verify with a DNS TXT record instead of HTTP | - | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages domains verify`

Prove ownership and issue the certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--domain-id <id>` | Custom domain ID (see `domains list`) | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages domains renew`

Renew the certificate; the current one keeps serving meanwhile (DNS-01: prints the new TXT record)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--domain-id <id>` | Custom domain ID (see `domains list`) | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages domains remove` (alias: `rm`)

Detach a hostname; it answers 404 immediately and its certificate is deleted

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--domain-id <id>` | Custom domain ID (see `domains list`) | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages subscriptions`

Let visitors subscribe to a status page by email

**Subcommands:**

- `show` - Show whether visitors can subscribe, the sender and what is missing
- `set` - Switch subscriptions on or off, or set the sender address

#### `status-pages subscriptions show`

Show whether visitors can subscribe, the sender and what is missing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages subscriptions set`

Switch subscriptions on or off, or set the sender address

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--enable` | Show the subscribe form on the public page | - | No |
| `--disable` | Hide the subscribe form and stop sending | - | No |
| `--sender <address>` | From address at a verified email domain ("" clears it) | - | No |
| `--json` | Output in JSON format | - | No |

### `status-pages subscribers`

List, export or delete the subscribers of a status page

**Subcommands:**

- `list` (`ls`) - List subscribers, newest first
- `export` - Export every subscriber as CSV (email,state,subscribed_at,confirmed_at)
- `delete` (`rm`) - Delete a subscriber; the address gets no more mail

#### `status-pages subscribers list` (alias: `ls`)

List subscribers, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--page <n>` | Page number | `1` | No |
| `--page-size <n>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages subscribers export`

Export every subscriber as CSV (email,state,subscribed_at,confirmed_at)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `-o, --output <file>` | Write to a file instead of standard output | - | No |

#### `status-pages subscribers delete` (alias: `rm`)

Delete a subscriber; the address gets no more mail

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--subscriber-id <id>` | Subscriber ID (see `subscribers list`) | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages maintenance`

Schedule maintenance windows: shown on the page, no outage alerts, no downtime counted

**Subcommands:**

- `list` (`ls`) - List maintenance windows, newest first
- `schedule` - Schedule a window; subscribers and webhooks are told once
- `update` - Change a window; once it has started only the title, description and end can change
- `cancel` - Cancel a scheduled window, or end a running one now

#### `status-pages maintenance list` (alias: `ls`)

List maintenance windows, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--page <n>` | Page number | `1` | No |
| `--page-size <n>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages maintenance schedule`

Schedule a window; subscribers and webhooks are told once

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--title <title>` | Public title (at most 120 characters) | - | Yes |
| `--description <text>` | Public description (at most 2000 characters) | - | No |
| `--starts <when>` | now, or an ISO 8601 date with a zone | - | Yes |
| `--ends <when>` | ISO 8601 date with a zone | - | No |
| `--duration <span>` | Instead of --ends: 90m, 2h, 1d (at most 7d) | - | No |
| `--monitor <id>` | Monitor on this page the window covers (repeat, or comma-separate) | - | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages maintenance update`

Change a window; once it has started only the title, description and end can change

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--maintenance-id <id>` | Maintenance ID (see `maintenance list`) | - | Yes |
| `--title <title>` | Public title | - | No |
| `--description <text>` | Public description ("" removes it) | - | No |
| `--starts <when>` | now, or an ISO 8601 date with a zone | - | No |
| `--ends <when>` | ISO 8601 date with a zone | - | No |
| `--duration <span>` | With --starts, instead of --ends: 90m, 2h, 1d | - | No |
| `--monitor <id>` | Replace the covered monitors (repeat, or comma-separate) | - | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages maintenance cancel`

Cancel a scheduled window, or end a running one now

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--maintenance-id <id>` | Maintenance ID (see `maintenance list`) | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages webhooks`

Post every public update of a status page to signed HTTPS webhooks

**Subcommands:**

- `get` - Show one webhook of a status page
- `list` (`ls`) - List the webhooks of a status page and how deliveries are going
- `add` - Add a webhook; its signing secret is printed once
- `update` - Change the URL, or pause and resume deliveries
- `remove` (`rm`) - Delete a webhook; events waiting for it are dropped

#### `status-pages webhooks get`

Show one webhook of a status page

**Usage:** `status-pages webhooks get <status-page-id> <webhook-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `status-pages webhooks list` (alias: `ls`)

List the webhooks of a status page and how deliveries are going

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages webhooks add`

Add a webhook; its signing secret is printed once

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--url <url>` | https:// URL with a public address | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages webhooks update`

Change the URL, or pause and resume deliveries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--webhook-id <id>` | Webhook ID (see `webhooks list`) | - | Yes |
| `--url <url>` | New https:// URL | - | No |
| `--enable` | Resume deliveries | - | No |
| `--disable` | Pause deliveries; those still waiting are dropped | - | No |
| `--json` | Output in JSON format | - | No |

#### `status-pages webhooks remove` (alias: `rm`)

Delete a webhook; events waiting for it are dropped

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--webhook-id <id>` | Webhook ID (see `webhooks list`) | - | Yes |
| `-y, --yes` | Skip the confirmation prompt | - | No |

### `status-pages password`

Protect a status page with a password

**Subcommands:**

- `show` - Show whether a status page is password-protected
- `set` - Set or change the password (prompted, or read from stdin); signs every visitor out

#### `status-pages password show`

Show whether a status page is password-protected

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `status-pages password set`

Set or change the password (prompted, or read from stdin); signs every visitor out

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Status page ID | - | Yes |
| `--password-stdin` | Read the password from the first line of stdin | - | No |
| `--remove` | Remove the password and open the page again | - | No |
| `--json` | Output in JSON format | - | No |

### `status-pages get-overview`

Get status page overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `status-pages get-maintenance`

A maintenance window

**Usage:** `status-pages get-maintenance <status-page-id> <maintenance-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `status-pages preview`

Preview what a status page shows visitors, published or not

**Usage:** `status-pages preview <status-page-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `status-pages get-public-feed`

Public status page (Atom feed)

**Usage:** `status-pages get-public-feed <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

### `status-pages get-public-json`

Public status page (JSON)

**Usage:** `status-pages get-public-json <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `alarms` (alias: `alarm`)

List, acknowledge, and resolve alarms (container crashes, uptime, metrics, databases)

**Subcommands:**

- `list` (`ls`) - List alarms, newest first
- `summary` - Show active alarm counts by status, severity, and type
- `ack` (`acknowledge`) - Acknowledge alarms by ID, or every alarm matching the filters with --all
- `resolve` - Resolve alarms by ID, or every alarm matching the filters with --all
- `acknowledge-alarm` - Acknowledge a firing alarm (marks it as seen but not resolved)
- `resolve-alarm` - Resolve an alarm
- `unsilence` - Lift an alarm's silence before it expires, so the next breach of the same alert notifies again
- `silence` - Silence an alarm (and future re-fires of the same type/scope) for a chosen duration, without permanently..
- `system` - System commands

### `alarms list` (alias: `ls`)

List alarms, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--project-id <id>` | Project ID (instead of --project) | - | No |
| `--system` | Target host-wide system alarms (disk space, worker nodes) instead of a project | - | No |
| `--status <status>` | Filter by status (firing, acknowledged, resolved) | - | No |
| `--severity <severity>` | Filter by severity (info, warning, critical) | - | No |
| `--type <type>` | Filter by alarm type (e.g. container_crash) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--deployment-id <id>` | Filter by deployment ID | - | No |
| `--service-id <id>` | Only alarms of this external service | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms summary`

Show active alarm counts by status, severity, and type

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--project-id <id>` | Project ID (instead of --project) | - | No |
| `--system` | Target host-wide system alarms (disk space, worker nodes) instead of a project | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms ack` (alias: `acknowledge`)

Acknowledge alarms by ID, or every alarm matching the filters with --all

**Usage:** `alarms ack [alarmIds...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--project-id <id>` | Project ID (instead of --project) | - | No |
| `--system` | Target host-wide system alarms (disk space, worker nodes) instead of a project | - | No |
| `--status <status>` | Filter by status (firing, acknowledged, resolved) | - | No |
| `--severity <severity>` | Filter by severity (info, warning, critical) | - | No |
| `--type <type>` | Filter by alarm type (e.g. container_crash) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--deployment-id <id>` | Filter by deployment ID | - | No |
| `--all` | Target every alarm matching the filters instead of explicit IDs | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms resolve`

Resolve alarms by ID, or every alarm matching the filters with --all

**Usage:** `alarms resolve [alarmIds...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--project-id <id>` | Project ID (instead of --project) | - | No |
| `--system` | Target host-wide system alarms (disk space, worker nodes) instead of a project | - | No |
| `--status <status>` | Filter by status (firing, acknowledged, resolved) | - | No |
| `--severity <severity>` | Filter by severity (info, warning, critical) | - | No |
| `--type <type>` | Filter by alarm type (e.g. container_crash) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--deployment-id <id>` | Filter by deployment ID | - | No |
| `--all` | Target every alarm matching the filters instead of explicit IDs | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms acknowledge-alarm`

Acknowledge a firing alarm (marks it as seen but not resolved)

**Usage:** `alarms acknowledge-alarm <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms resolve-alarm`

Resolve an alarm

**Usage:** `alarms resolve-alarm <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms unsilence`

Lift an alarm's silence before it expires, so the next breach of the same alert notifies again

**Usage:** `alarms unsilence <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms silence`

Silence an alarm (and future re-fires of the same type/scope) for a chosen duration, without permanently..

**Usage:** `alarms silence <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--duration-hours <n>` | How long to mute this alarm (and future re-fires of the same type/scope) for, in hours [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms system`

System commands

**Subcommands:**

- `acknowledge` - Acknowledge a firing system alarm
- `resolve` - Resolve a system alarm
- `unsilence` - Lift a system alarm's silence before it expires
- `silence` - Silence a system alarm (and future re-fires of the same type/scope) for a chosen duration, without..

#### `alarms system acknowledge`

Acknowledge a firing system alarm

**Usage:** `alarms system acknowledge <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `alarms system resolve`

Resolve a system alarm

**Usage:** `alarms system resolve <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `alarms system unsilence`

Lift a system alarm's silence before it expires

**Usage:** `alarms system unsilence <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `alarms system silence`

Silence a system alarm (and future re-fires of the same type/scope) for a chosen duration, without..

**Usage:** `alarms system silence <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--duration-hours <n>` | How long to mute this alarm (and future re-fires of the same type/scope) for, in hours [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

## `emails` (alias: `email`)

Manage and send emails

**Subcommands:**

- `list` (`ls`) - List sent emails
- `send` - Send an email
- `show` - Show email details
- `stats` - Get email statistics
- `validate` - Validate an email address
- `tracking` - Tracking commands
- `resend` - Resend a captured email

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
| `--cc <value>` | CC recipient (repeatable) (repeatable) | - | No |
| `--bcc <value>` | BCC recipient (repeatable) (repeatable) | - | No |
| `--reply-to <value>` | Reply-to address | - | No |
| `--html <value>` | HTML body, sent alongside the text body | - | No |
| `--headers <json>` | Extra headers as a JSON object, e.g. {"X-Campaign":"spring"} (JSON) | - | No |

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

### `emails tracking`

Tracking commands

**Subcommands:**

- `get-global-events` - GET /emails/events
- `get-global-event-stats` - GET /emails/events/stats
- `get` - Get email tracking summary
- `get-events` - Get email tracking events
- `get-links` - Get tracked links for an email

#### `emails tracking get-global-events`

GET /emails/events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-type <value>` | Filter by event type (open, click) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `emails tracking get-global-event-stats`

GET /emails/events/stats

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `emails tracking get`

Get email tracking summary

**Usage:** `emails tracking get <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `emails tracking get-events`

Get email tracking events

**Usage:** `emails tracking get-events <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-type <value>` | Filter by event type (open, click) | - | No |
| `--json` | Output in JSON format | - | No |

#### `emails tracking get-links`

Get tracked links for an email

**Usage:** `emails tracking get-links <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `emails resend`

Resend a captured email

**Usage:** `emails resend <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
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
| `--type <type>` | http (default: match the Host header) or tls (match the SNI name, pass TCP through) | - | No |
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
| `--type <type>` | http (match the Host header) or tls (match the SNI name, pass TCP through) | - | No |

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
| `--name-pattern <value>` | Only workloads whose name matches this glob or pattern | - | No |
| `--status-filter <value>` | Only workloads in this state, e.g. running (repeatable) (repeatable) | - | No |
| `--workload-type-filter <value>` | Only workloads of this type, e.g. container or static-site (repeatable) (repeatable) | - | No |
| `--label-filter <json>` | Only workloads with these labels, as a JSON object, e.g. {"env":"prod"} (JSON) | - | No |
| `--limit <n>` | Return at most this many workloads | - | No |
| `--token-file <path>` | Read the source API token or admin password from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the source API token or admin password from stdin instead of --token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
| `--name-pattern <value>` | Only workloads whose name matches this glob or pattern | - | No |
| `--status-filter <value>` | Only workloads in this state, e.g. running (repeatable) (repeatable) | - | No |
| `--workload-type-filter <value>` | Only workloads of this type, e.g. container or static-site (repeatable) (repeatable) | - | No |
| `--label-filter <json>` | Only workloads with these labels, as a JSON object, e.g. {"env":"prod"} (JSON) | - | No |
| `--limit <n>` | Return at most this many workloads | - | No |
| `--repository-id <n>` | Connected repository to detect the build preset from | - | No |
| `--token-file <path>` | Read the source API token or admin password from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the source API token or admin password from stdin instead of --token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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
| `--name-pattern <value>` | Only workloads whose name matches this glob or pattern | - | No |
| `--status-filter <value>` | Only workloads in this state, e.g. running (repeatable) (repeatable) | - | No |
| `--workload-type-filter <value>` | Only workloads of this type, e.g. container or static-site (repeatable) (repeatable) | - | No |
| `--label-filter <json>` | Only workloads with these labels, as a JSON object, e.g. {"env":"prod"} (JSON) | - | No |
| `--limit <n>` | Return at most this many workloads | - | No |
| `--repository-id <n>` | Connected repository to detect the build preset from | - | No |
| `--token-file <path>` | Read the source API token or admin password from a file (- reads stdin) instead of --token | - | No |
| `--token-stdin` | Read the source API token or admin password from stdin instead of --token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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

Browse templates and create them

**Subcommands:**

- `list` (`ls`) - List available templates
- `create` - Create a template: every service (an app, what `projects` manages), the databases it needs, its shared variables and path routes
- `validate` - Validate a Temps-native template YAML file or directory offline
- `list-tags` - List all available template tags

### `templates list` (alias: `ls`)

List available templates

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--kind <kind>` | Filter by kind: starter or app | - | No |
| `--featured` | Only featured templates | - | No |
| `--tag <tag>` | Only templates with this tag (see: templates list-tags) | - | No |

### `templates create`

Create a template: every service (an app, what `projects` manages), the databases it needs, its shared variables and path routes

**Usage:** `templates create <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <name>` | Put the services in a new project (group) with this name; the default for several services | - | No |
| `--into <project>` | Add the services to an existing project (group), by slug or id (see: project-groups list) | - | No |
| `--standalone` | Create a one-service template without a project (group); the default for one service | - | No |
| `--name <service=name...>` | Name a created service; a one-service template also takes a bare name | - | No |
| `--env <[service:]KEY=VALUE...>` | Set a variable; prefix the service when the template cannot tell | - | No |
| `--secret <[service:]KEY=VALUE...>` | Set a secret variable (write-only once stored); the value is visible in shell history, so prefer --secret-file | - | No |
| `--secret-file <[service:]KEY=PATH...>` | Set a secret variable from a file, byte-for-byte (PATH - reads stdin) | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline from each --secret-file value | - | No |
| `--database <type=new\|type=new:name\|type=id...>` | Create or link each database the template needs (default: create) | - | No |
| `--fork-connection <id>` | Starters only: copy the code into a new repository on this git connection | - | No |
| `--repo-name <name>` | Name of the new repository (with --fork-connection) | - | No |
| `--repo-owner <owner>` | Account or organization for the new repository | - | No |
| `--public-repo` | Make the new repository public (private by default) | - | No |
| `--no-auto-deploy` | Do not deploy on push to the new repository | - | No |
| `--idempotency-key <key>` | Reuse a key to get the same result instead of creating twice | - | No |
| `--runtime-overrides <json>` | Image, command, port, health check or limits per image service, e.g. {"api":{"exposed_port":8080}} | - | No |
| `--no-wait` | Return right away instead of waiting until it finishes | - | No |
| `--timeout <minutes>` | Stop waiting after this long (default 30); it keeps running on the server | - | No |
| `--json` | Output in JSON format | - | No |

### `templates validate`

Validate a Temps-native template YAML file or directory offline

**Usage:** `templates validate <path> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `templates list-tags`

List all available template tags

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `platform` (alias: `plat`)

View platform and server information

**Subcommands:**

- `operations` (`ops`) - List recent and in-flight operations: deployments, rollbacks, promotions, restores, backups, autofixes
- `maturity` - Show the compatibility promise (stable, beta, experimental) for every feature
- `geo` - Look up the location of an IP address in the server's geolocation database
- `file` - Print a file from the server's static directory (or save it with -o)
- `info` - Get platform information
- `features` - Show which capabilities the server process actually provides
- `access` - Get access and networking information
- `private-ip` - Get the server private IP address
- `public-ip` - Get the server public IP address
- `update` - Check for and apply temps releases on the server
- `alert-rules` - Inspect and retune the control-plane's own monitoring alert rules

### `platform operations` (alias: `ops`)

List recent and in-flight operations: deployments, rollbacks, promotions, restores, backups, autofixes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <status>` | Which ones: running, waiting, finished, all (default: the server's) | - | No |
| `--kind <kind>` | Only one kind: deployment, rollback, promotion, restore, backup, autofix | - | No |
| `--project <id>` | Only this project ID | - | No |
| `--page <n>` | Page (default 1) | - | No |
| `--page-size <n>` | Operations per page (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `platform maturity`

Show the compatibility promise (stable, beta, experimental) for every feature

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `platform geo`

Look up the location of an IP address in the server's geolocation database

**Usage:** `platform geo <ip> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `platform file`

Print a file from the server's static directory (or save it with -o)

**Usage:** `platform file <path> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <file>` | Write it to a file instead of stdout | - | No |

### `platform info`

Get platform information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `platform features`

Show which capabilities the server process actually provides

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

**Usage:** `platform update channel [channel] [options]`

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

**Usage:** `platform alert-rules set <rule-id> [options]`

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

**Usage:** `presets show <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `analytics` (alias: `stats`)

View project analytics

**Subcommands:**

- `keys` - Manage analytics ingest keys (pa_...) for apps Temps does not deploy
- `conversions` (`goals`) - Conversion goals: events or pages that count as success
- `identity` - Whether visitors are recognised by a stored id or tracked cookieless, and who may identify them
- `overview` (`o`) - Show analytics dashboard overview
- `top` - Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign
- `outbound` - Show the outbound links visitors clicked (outbound_click events)
- `downloads` - Show the files visitors downloaded (file_download events)
- `not-found` - Show pages visitors could not find: 404s Temps answered and not_found events
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
- `enrich` - Attach identity or attributes to a visitor (merges into custom_data; a null value removes a key)
- `identify` - Label a visitor with your app's user id, as your backend would (browser identify cannot replace it)
- `get-active-visitors` - Get detailed active visitors
- `get-event-detail` - Get detailed analytics for a specific event
- `get-event-entries` - Get paginated list of raw occurrences of a specific event, including custom JSON properties
- `get-event-visitors` - Get paginated list of visitors who triggered a specific event
- `get-events-count` - Get analytics events count
- `get-stats` - Get general statistics across all projects for a time frame
- `get-global` - Get global analytics
- `get` - Get analytics projects
- `check-has-events` - Check analytics has events
- `get-live-visitors-list` - Get list of currently live visitors from visitor table
- `get-page-flow` - Get page flow analytics: entry pages, exit pages, drop-off points, and page transitions
- `get-page-hourly-sessions` - Get page hourly sessions
- `get-page-path-detail` - Get detailed analytics for a specific page path Returns visitors, page views, activity over time,..
- `get-page-path-visitors` - Get individual visitor sessions for a specific page path
- `get-page-paths-sparklines` - Get page paths sparklines
- `get-recent-activity` - Get recent activity events for real-time activity feed
- `get-session-details` - Get detailed information about a specific session including events and request logs
- `get-session-events` - Get analytics session events
- `get-session-logs` - Get session logs
- `get-visitor-facets` - Get filter dropdown contents for the visitors page
- `get-visitors` - Get list of visitors with summary information
- `get-visitor-by-guid` - Get visitor by GUID with geolocation data
- `get-visitor-by-id` - Get visitor by numeric ID with geolocation data
- `get-visitor-details` - Get detailed information about a specific visitor by numeric ID
- `identify-visitor-for-token` - Identify a visitor of the token's own project (ADR 109 D4)
- `get-visitor-info` - Get visitor record from database
- `get-visitor-journey` - Get the complete visitor journey: all events across all sessions, grouped by session
- `get-visitor-sessions` - Get all sessions for a specific visitor by numeric ID
- `get-visitor-stats` - Get visitor statistics
- `events` - Events commands
- `visitors` - Visitors commands
- `settings` - Settings commands
- `get-api-callers` - Return the top callers (by client IP, ranked by request count) in a project's API traffic window
- `get-api-routes` - Return the top routes (by request count) in a project's API traffic, grouped by raw '(method, path)'

### `analytics keys`

Manage analytics ingest keys (pa_...) for apps Temps does not deploy

**Subcommands:**

- `get` - Show one analytics ingest key
- `list` (`ls`) - List analytics ingest keys for a project
- `create` (`add`) - Mint a new analytics ingest key
- `update` - Update an ingest key's label, origin allowlist, or rate limit
- `rotate` - Replace an ingest key value, keeping the same row and scope
- `revoke` - Revoke (deactivate) an analytics ingest key
- `get-ingest-endpoint` - The public address an app Temps does not deploy should send analytics to

#### `analytics keys get`

Show one analytics ingest key

**Usage:** `analytics keys get <key-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys list` (alias: `ls`)

List analytics ingest keys for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys create` (alias: `add`)

Mint a new analytics ingest key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Operator-facing label for the key | - | No |
| `--environment-id <id>` | Scope the key to one environment (omit for a project-wide key) | - | No |
| `--allowed-origins <origins...>` | Browser origins allowed to use this key (omit to allow any origin) | - | No |
| `--rate-limit <n>` | Requests per minute (omit for the server default; 0 or less for unlimited) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys update`

Update an ingest key's label, origin allowlist, or rate limit

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--key-id <id>` | Analytics ingest key ID | - | Yes |
| `-n, --name <name>` | New operator-facing label | - | No |
| `--allowed-origins <origins...>` | Replace the origin allowlist with these origins | - | No |
| `--clear-origins` | Clear the origin allowlist (allow any origin) | - | No |
| `--rate-limit <n>` | New requests-per-minute limit | - | No |
| `--clear-rate-limit` | Clear the rate limit (unlimited) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys rotate`

Replace an ingest key value, keeping the same row and scope

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--key-id <id>` | Analytics ingest key ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys revoke`

Revoke (deactivate) an analytics ingest key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--key-id <id>` | Analytics ingest key ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

#### `analytics keys get-ingest-endpoint`

The public address an app Temps does not deploy should send analytics to

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics conversions` (alias: `goals`)

Conversion goals: events or pages that count as success

**Subcommands:**

- `list` - List conversions with visitors, rate and revenue in a period
- `create` - Define a conversion
- `update` - Replace a conversion's definition
- `delete` - Delete a conversion (recorded events are kept)
- `breakdown` - Where the visitors who converted came from
- `get-goal` - Get one conversion goal

#### `analytics conversions list`

List conversions with visitors, rate and revenue in a period

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 7d, 30d, 3m) | `30d` | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

#### `analytics conversions create`

Define a conversion

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Name shown in reports, e.g. Signup | - | No |
| `--event <name>` | Count this event, e.g. signup | - | No |
| `--page <path>` | Count visits to this path; * matches anything, e.g. /docs/* | - | No |
| `--property <key=value>` | Only events whose property has this value, e.g. plan=pro | - | No |
| `--revenue-property <key>` | Event property holding the amount, e.g. amount | - | No |
| `--currency <code>` | ISO 4217 currency of the amount, e.g. EUR | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics conversions update`

Replace a conversion's definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--goal-id <id>` | Conversion ID (see list) | - | Yes |
| `-n, --name <name>` | Name shown in reports, e.g. Signup | - | No |
| `--event <name>` | Count this event, e.g. signup | - | No |
| `--page <path>` | Count visits to this path; * matches anything, e.g. /docs/* | - | No |
| `--property <key=value>` | Only events whose property has this value, e.g. plan=pro | - | No |
| `--revenue-property <key>` | Event property holding the amount, e.g. amount | - | No |
| `--currency <code>` | ISO 4217 currency of the amount, e.g. EUR | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics conversions delete`

Delete a conversion (recorded events are kept)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--goal-id <id>` | Conversion ID (see list) | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

#### `analytics conversions breakdown`

Where the visitors who converted came from

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--goal-id <id>` | Conversion ID (see list) | - | Yes |
| `--by <dimension>` | One of channel, referrer, utm_source, utm_medium, utm_campaign, page, country, device_type, browser, operating_system | `channel` | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 7d, 30d, 3m) | `30d` | No |
| `--limit <n>` | Rows (default: 10, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

#### `analytics conversions get-goal`

Get one conversion goal

**Usage:** `analytics conversions get-goal <goal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics identity`

Whether visitors are recognised by a stored id or tracked cookieless, and who may identify them

**Subcommands:**

- `show` - Show the identity mode, whether browser identify is accepted, and what they change
- `set` - Switch between persistent and cookieless tracking, or accept/refuse browser identify

#### `analytics identity show`

Show the identity mode, whether browser identify is accepted, and what they change

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics identity set`

Switch between persistent and cookieless tracking, or accept/refuse browser identify

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--mode <mode>` | One of persistent, cookieless | - | No |
| `--browser-identify <accept\|refuse>` | Whether identify() from the browser is applied; refuse leaves identify to the app's backend | - | No |
| `-y, --yes` | Skip confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics overview` (alias: `o`)

Show analytics dashboard overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |
| `--deployment-id <n>` | Only this deployment (an ID) | - | No |

### `analytics top`

Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign

**Usage:** `analytics top <dimension> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |
| `--deployment-id <n>` | Only this deployment (an ID) | - | No |
| `--filter-browser <value>` | Only visitors on this browser | - | No |
| `--filter-channel <value>` | Only visitors from this channel (e.g. Organic Search) | - | No |
| `--filter-country <value>` | Only visitors from this country | - | No |
| `--filter-os <value>` | Only visitors on this operating system | - | No |
| `--filter-referrer <value>` | Only visitors from this referrer | - | No |
| `--filter-region <value>` | Only visitors from this region | - | No |
| `--include-crawlers` | Count crawler traffic too | - | No |
| `--no-include-crawlers` | Set include_crawlers to false | - | No |

### `analytics outbound`

Show the outbound links visitors clicked (outbound_click events)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics downloads`

Show the files visitors downloaded (file_download events)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics not-found`

Show pages visitors could not find: 404s Temps answered and not_found events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics funnels`

Show funnel conversion metrics for all funnels

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |
| `--country-code <value>` | Only visitors from this country (ISO code, e.g. DE) | - | No |

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

**Subcommands:**

- `has-metrics` - Check if performance metrics exist for a project

#### `analytics performance has-metrics`

Check if performance metrics exist for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
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
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

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
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics ai-page`

Show which agents/providers crawled a single page (e.g. /docs)

**Usage:** `analytics ai-page <path> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Number of rows to fetch (default: 50, max: 100) | - | No |
| `--group-by <mode>` | Group rows by "agent" (default) or "provider" | `agent` | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

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

**Usage:** `analytics api-ip <ip> [options]`

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

**Usage:** `analytics api-path <path> [options]`

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
| `--refresh` | Generate a fresh summary instead of the cached one | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics enrich`

Attach identity or attributes to a visitor (merges into custom_data; a null value removes a key)

**Usage:** `analytics enrich <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --data <json>` | JSON object to merge, e.g. '{"user_id":"user_123"}' | - | No |
| `-f, --file <path>` | Read the JSON object to merge from a file | - | No |
| `--set <key=value>` | Set one key to a string value (repeatable; use --data for numbers, booleans, or nested values) | `` | No |
| `--unset <key>` | Remove one top-level key (repeatable; sends null) | `` | No |
| `--json` | Output in JSON format | - | No |

### `analytics identify`

Label a visitor with your app's user id, as your backend would (browser identify cannot replace it)

**Usage:** `analytics identify <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--user-id <id>` | Your app's id for this person (at most 256 characters) | - | Yes |
| `-d, --data <json>` | Traits as a JSON object, e.g. '{"plan":"pro"}' | - | No |
| `--set <key=value>` | Set one trait to a string value (repeatable) | `` | No |
| `--unset <key>` | Remove one trait (repeatable; sends null) | `` | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-active-visitors`

Get detailed active visitors

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--deployment-id <n>` | Deployment ID (optional) | - | No |
| `--window-minutes <n>` | Time window in minutes for active visitors (default: 5) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-event-detail`

Get detailed analytics for a specific event

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-name <value>` | Event name to get details for [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date (ISO 8601) [required] | - | No |
| `--end-date <value>` | End date (ISO 8601) [required] | - | No |
| `--bucket-interval <value>` | Bucket interval: hour, day, week, month (default: auto) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-event-entries`

Get paginated list of raw occurrences of a specific event, including custom JSON properties

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-name <value>` | Event name to list occurrences for [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date (ISO 8601) [required] | - | No |
| `--end-date <value>` | End date (ISO 8601) [required] | - | No |
| `--page <n>` | Page number (1-based, default: 1) | - | No |
| `--per-page <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-event-visitors`

Get paginated list of visitors who triggered a specific event

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-name <value>` | Event name to list visitors for [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date (ISO 8601) [required] | - | No |
| `--end-date <value>` | End date (ISO 8601) [required] | - | No |
| `--page <n>` | Page number (1-based, default: 1) | - | No |
| `--per-page <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-events-count`

Get analytics events count

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--limit <n>` | Maximum number of results to return | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--custom-events-only` | Only return custom events, excluding system events like page_view, page_leave, heartbeat (default: true) | - | No |
| `--no-custom-events-only` | Set custom_events_only to false | - | No |
| `--breakdown <value>` | Breakdown by geography: 'country', 'region', or 'city' (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-stats`

Get general statistics across all projects for a time frame

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--project-ids <n>` | Optional: Filter by specific project IDs (comma-separated) (repeatable) | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--include-project-breakdown` | Whether to include per-project breakdown (default: false) | - | No |
| `--no-include-project-breakdown` | Set include_project_breakdown to false | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-global`

Get global analytics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--facet <value>` | Query parameter facet (one of: summary, traffic, pages, events, breakdown, speed) | - | No |
| `--project-id <n>` | Query parameter project_id | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--start-date <value>` | Query parameter start_date [required] | - | No |
| `--end-date <value>` | Query parameter end_date [required] | - | No |
| `--search <value>` | Query parameter search | - | No |
| `--dimension <value>` | Query parameter dimension | - | No |
| `--device <value>` | Query parameter device | - | No |
| `--sort-by <value>` | Query parameter sort_by | - | No |
| `--sort-order <value>` | Query parameter sort_order | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--per-page <n>` | Query parameter per_page | - | No |
| `--min-views <n>` | Query parameter min_views | - | No |
| `--min-sessions <n>` | Query parameter min_sessions | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get`

Get analytics projects

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `analytics check-has-events`

Check analytics has events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-live-visitors-list`

Get list of currently live visitors from visitor table

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--window-minutes <n>` | Time window in minutes for live visitors (default: 5) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-flow`

Get page flow analytics: entry pages, exit pages, drop-off points, and page transitions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in ISO 8601 format [required] | - | No |
| `--end-date <value>` | End date in ISO 8601 format [required] | - | No |
| `--limit <n>` | Max entry/exit pages to return (default: 20, max: 100) | - | No |
| `--transitions-limit <n>` | Max page transitions to return (default: 50, max: 200) | - | No |
| `--min-views-for-dropoff <n>` | Minimum views for drop-off analysis (default: 5) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-hourly-sessions`

Get page hourly sessions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page-path <value>` | The page path to get sessions for [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-time <value>` | Start time in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-time <value>` | End time in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--bucket-interval <value>` | Bucket interval: 'hour', 'day', 'week', or 'month' (default: auto-determined based on range) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-path-detail`

Get detailed analytics for a specific page path Returns visitors, page views, activity over time,..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page-path <value>` | The page path to get details for (URL-encoded) [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in ISO 8601 format [required] | - | No |
| `--end-date <value>` | End date in ISO 8601 format [required] | - | No |
| `--bucket-interval <value>` | Bucket interval for time series: 'hour', 'day', 'week', 'month' (default: auto based on date range) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-path-visitors`

Get individual visitor sessions for a specific page path

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page-path <value>` | The page path to get visitors for (URL-encoded) [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in ISO 8601 format [required] | - | No |
| `--end-date <value>` | End date in ISO 8601 format [required] | - | No |
| `--page <n>` | Page number (1-based, default: 1) | - | No |
| `--per-page <n>` | Items per page (default: 50, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-paths-sparklines`

Get page paths sparklines

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-time <value>` | Start time in ISO 8601 format [required] | - | No |
| `--end-time <value>` | End time in ISO 8601 format [required] | - | No |
| `--page-paths <value>` | Comma-separated list of page paths [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-recent-activity`

Get recent activity events for real-time activity feed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--since-id <n>` | Return events with ID greater than this (cursor-based polling) | - | No |
| `--limit <n>` | Max events to return (default: 50, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-session-details`

Get detailed information about a specific session including events and request logs

**Usage:** `analytics get-session-details <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-session-events`

Get analytics session events

**Usage:** `analytics get-session-events <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS | - | No |
| `--limit <n>` | Number of results to return (default: 100) | - | No |
| `--offset <n>` | Number of results to skip (default: 0) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-session-logs`

Get session logs

**Usage:** `analytics get-session-logs <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS | - | No |
| `--limit <n>` | Number of results to return (default: 100) | - | No |
| `--offset <n>` | Number of results to skip (default: 0) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-facets`

Get filter dropdown contents for the visitors page

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--include-crawlers` | Include crawlers (default: false) | - | No |
| `--no-include-crawlers` | Set include_crawlers to false | - | No |
| `--has-activity-only` | Hide ghost visitors (default: true) | - | No |
| `--no-has-activity-only` | Set has_activity_only to false | - | No |
| `--per-facet-limit <n>` | Top N values per dimension (default: 50, max: 200) | - | No |
| `--filter-country <value>` | Geolocation country | - | No |
| `--filter-region <value>` | Geolocation region | - | No |
| `--filter-city <value>` | Geolocation city | - | No |
| `--filter-channel <value>` | First-touch channel | - | No |
| `--filter-referrer <value>` | First-touch referrer hostname (use 'Direct' for null) | - | No |
| `--filter-user-id <value>` | User id set by identify(): the same user on every device | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitors`

Get list of visitors with summary information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--include-crawlers` | Include crawlers (default: false) | - | No |
| `--no-include-crawlers` | Set include_crawlers to false | - | No |
| `--limit <n>` | Maximum number of visitors to return (default: 50) | - | No |
| `--offset <n>` | Number of visitors to skip (default: 0) | - | No |
| `--has-activity-only` | Filter to only include visitors with recorded activity (events/sessions) | - | No |
| `--no-has-activity-only` | Set has_activity_only to false | - | No |
| `--filter-country <value>` | Geolocation country | - | No |
| `--filter-region <value>` | Geolocation region | - | No |
| `--filter-city <value>` | Geolocation city | - | No |
| `--filter-channel <value>` | First-touch channel | - | No |
| `--filter-referrer <value>` | First-touch referrer hostname (use 'Direct' for null) | - | No |
| `--filter-user-id <value>` | User id set by identify(): the same user on every device | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-by-guid`

Get visitor by GUID with geolocation data

**Usage:** `analytics get-visitor-by-guid <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-by-id`

Get visitor by numeric ID with geolocation data

**Usage:** `analytics get-visitor-by-id <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-details`

Get detailed information about a specific visitor by numeric ID

**Usage:** `analytics get-visitor-details <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics identify-visitor-for-token`

Identify a visitor of the token's own project (ADR 109 D4)

**Usage:** `analytics identify-visitor-for-token <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--traits <json>` | Traits merged into the visitor's 'custom_data' at the top level; a key set to 'null' is removed (JSON) | - | No |
| `--user-id <value>` | The app's own id for the person (database id, UUID, email) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-info`

Get visitor record from database

**Usage:** `analytics get-visitor-info <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-journey`

Get the complete visitor journey: all events across all sessions, grouped by session

**Usage:** `analytics get-visitor-journey <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--limit-sessions <n>` | Maximum number of sessions to return (default: 50) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-sessions`

Get all sessions for a specific visitor by numeric ID

**Usage:** `analytics get-visitor-sessions <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--limit <n>` | Maximum number of sessions to return (default: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-stats`

Get visitor statistics

**Usage:** `analytics get-visitor-stats <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics events`

Events commands

**Subcommands:**

- `get-dashboard` - Get dashboard analytics for multiple projects in a single batch request
- `get-active-visitors` - Get active visitors count
- `get-type-breakdown` - Get event type breakdown
- `record-console` - Record an analytics event via the console API with explicit project ID
- `get-property-timeline` - Get property timeline by grouping events by a column over time
- `get-timeline` - Get events timeline
- `has` - Check if project has any analytics events
- `get-hourly-visits` - Get hourly visits
- `get-session` - Get events for a specific session

#### `analytics events get-dashboard`

Get dashboard analytics for multiple projects in a single batch request

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-ids <value>` | Comma-separated list of project IDs [required] | - | No |
| `--start-date <value>` | Start date for filtering [required] | - | No |
| `--end-date <value>` | End date for filtering [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-active-visitors`

Get active visitors count

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--deployment-id <n>` | Filter by deployment ID | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-type-breakdown`

Get event type breakdown

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <value>` | Start date for filtering events [required] | - | No |
| `--end-date <value>` | End date for filtering events [required] | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--aggregation-level <value>` | Aggregation level: events, sessions, or visitors (default: events) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events record-console`

Record an analytics event via the console API with explicit project ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--deployment-id <n>` | Deployment ID to attribute the event to [required] | - | No |
| `--environment-id <n>` | Environment ID to attribute the event to [required] | - | No |
| `--event-data <json>` | Arbitrary JSON event data (JSON) | - | No |
| `--event-name <value>` | Event name (e.g. "purchase", "signup", custom event names) [required] | - | No |
| `--request-path <value>` | Page path context (defaults to "/") | - | No |
| `--request-query <value>` | Query string context | - | No |
| `--session-id <value>` | Encrypted '_temps_sid' cookie value from the user's browser | - | No |
| `--visitor-id <value>` | Encrypted '_temps_visitor_id' cookie value from the user's browser | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-property-timeline`

Get property timeline by grouping events by a column over time

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <value>` | Start date in '%Y-%m-%d %H:%M:%S' format [required] | - | No |
| `--end-date <value>` | End date in '%Y-%m-%d %H:%M:%S' format [required] | - | No |
| `--group-by <value>` | Column to group by (channel, device_type, browser, etc.) [required] | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--deployment-id <n>` | Filter by deployment ID | - | No |
| `--event-name <value>` | Filter by event name | - | No |
| `--aggregation-level <value>` | Aggregation level: events, sessions, or visitors - default: events | - | No |
| `--bucket-size <value>` | Time bucket: hour, day, week, month (default: auto-detect) | - | No |
| `--include-crawlers` | Include crawler/bot traffic (default: false) | - | No |
| `--no-include-crawlers` | Set include_crawlers to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-timeline`

Get events timeline

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <value>` | Start date for filtering events [required] | - | No |
| `--end-date <value>` | End date for filtering events [required] | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--event-name <value>` | Filter by specific event name | - | No |
| `--bucket-size <value>` | Bucket size: hour, day, or week (auto-detected if not specified) | - | No |
| `--aggregation-level <value>` | Aggregation level: events, sessions, or visitors (default: events) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events has`

Check if project has any analytics events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-hourly-visits`

Get hourly visits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <value>` | Start date for filtering visits [required] | - | No |
| `--end-date <value>` | End date for filtering visits [required] | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--aggregation-level <value>` | Aggregation level: events (page views), sessions (unique sessions), or visitors (unique visitors) -... | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-session`

Get events for a specific session

**Usage:** `analytics events get-session <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics visitors`

Visitors commands

**Subcommands:**

- `get-activity-status` - Get activity status
- `save-activity-settings` - Save activity settings
- `suggest-activity-goals` - Suggest activity goals
- `preview-activity-report` - Preview activity report
- `run-activity-report` - Run activity report

#### `analytics visitors get-activity-status`

Get activity status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics visitors save-activity-settings`

Save activity settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--application-context <value>` | Body field application_context [required] | - | No |
| `--categories <json>` | Body field categories (JSON) [required] | - | No |
| `--daily-enabled` | Body field daily_enabled [required] | - | No |
| `--no-daily-enabled` | Set daily_enabled to false | - | No |
| `--environment-id <n>` | Body field environment_id | - | No |
| `--goal-title <value>` | Short operator-facing label for the selected analysis goal | - | No |
| `--min-page-paths <n>` | Body field min_page_paths | - | No |
| `--min-sessions <n>` | Body field min_sessions | - | No |
| `--property-keys <value>` | Only these event-property keys may be sent to the provider (repeatable) [required] | - | No |
| `--share-activity-with-ai` | Explicit permission to send the selected analytics fields to the configured AI provider [required] | - | No |
| `--no-share-activity-with-ai` | Set share_activity_with_ai to false | - | No |
| `--source-domain <value>` | Body field source_domain | - | No |
| `--source-url <value>` | Body field source_url | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics visitors suggest-activity-goals`

Suggest activity goals

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Body field environment_id | - | No |
| `--share-with-ai` | Body field share_with_ai [required] | - | No |
| `--no-share-with-ai` | Set share_with_ai to false | - | No |
| `--url <value>` | Body field url [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics visitors preview-activity-report`

Preview activity report

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Body field environment_id | - | No |
| `--goal <value>` | Body field goal [required] | - | No |
| `--min-page-paths <n>` | Body field min_page_paths | - | No |
| `--min-sessions <n>` | Body field min_sessions | - | No |
| `--property-keys <value>` | Body field property_keys (repeatable) | - | No |
| `--share-activity-with-ai` | Body field share_activity_with_ai [required] | - | No |
| `--no-share-activity-with-ai` | Set share_activity_with_ai to false | - | No |
| `--source-domain <value>` | Body field source_domain | - | No |
| `--source-url <value>` | Body field source_url | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics visitors run-activity-report`

Run activity report

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics settings`

Settings commands

**Subcommands:**

- `get-filtered-traffic` - How many events were counted and how many were kept out of the project's analytics, by reason, over the..
- `get` - Get a project's analytics traffic settings and whether datacenter detection is available on this instance
- `update` - Change a project's analytics traffic settings
- `list-traffic-filters` - List a project's traffic filters, newest first
- `create-traffic-filter` - Add a traffic filter
- `delete-traffic-filter` - Remove a traffic filter

#### `analytics settings get-filtered-traffic`

How many events were counted and how many were kept out of the project's analytics, by reason, over the..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--days <n>` | Window in days, ending now (default 7, max 30) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings get`

Get a project's analytics traffic settings and whether datacenter detection is available on this instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings update`

Change a project's analytics traffic settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--exclude-datacenter-traffic` | Body field exclude_datacenter_traffic | - | No |
| `--no-exclude-datacenter-traffic` | Set exclude_datacenter_traffic to false | - | No |
| `--exclude-spam-referrers` | Body field exclude_spam_referrers | - | No |
| `--no-exclude-spam-referrers` | Set exclude_spam_referrers to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings list-traffic-filters`

List a project's traffic filters, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--kind <value>` | Only filters of this kind (one of: referrer, ip, asn, country) | - | No |
| `--page <n>` | Page number, starting at 1 (default 1) | - | No |
| `--page-size <n>` | Page size (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings create-traffic-filter`

Add a traffic filter

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--kind <value>` | What a traffic filter matches on (one of: referrer, ip, asn, country) [required] | - | No |
| `--note <value>` | Why the filter exists, for whoever reviews it later | - | No |
| `--value <value>` | A domain or URL ('referrer'), an IP address or CIDR range ('ip'), a network number with or without the... [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings delete-traffic-filter`

Remove a traffic filter

**Usage:** `analytics settings delete-traffic-filter <filter-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-api-callers`

Return the top callers (by client IP, ranked by request count) in a project's API traffic window

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Window start (ISO 8601) [required] | - | No |
| `--end-date <value>` | Window end (ISO 8601) [required] | - | No |
| `--limit <n>` | Max callers to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Ranked callers to skip (default: 0) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-api-routes`

Return the top routes (by request count) in a project's API traffic, grouped by raw '(method, path)'

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Window start (ISO 8601) [required] | - | No |
| `--end-date <value>` | Window end (ISO 8601) [required] | - | No |
| `--limit <n>` | Max routes to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Ranked routes to skip (default: 0) | - | No |
| `--json` | Output in JSON format | - | No |

## `funnels` (alias: `funnel`)

Manage analytics funnels for projects

**Subcommands:**

- `list` (`ls`) - List the funnels of a project, newest first
- `create` (`add`) - Create a new funnel for a project
- `update` - Update a funnel
- `remove` (`rm`) - Delete a funnel
- `metrics` - Get funnel metrics
- `preview` - Preview funnel metrics without saving
- `get-unique-events` - Get all unique/distinct event types for a project (paginated)

### `funnels list` (alias: `ls`)

List the funnels of a project, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
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
| `--start-date <value>` | Window start (RFC 3339) | - | No |
| `--end-date <value>` | Window end (RFC 3339) | - | No |
| `--environment-id <n>` | Only this environment (an ID) | - | No |
| `--country-code <value>` | Only visitors from this country (ISO code, e.g. DE) | - | No |

### `funnels preview`

Preview funnel metrics without saving

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-s, --steps <json>` | Funnel steps as JSON array | - | Yes |
| `--json` | Output in JSON format | - | No |

### `funnels get-unique-events`

Get all unique/distinct event types for a project (paginated)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 50, max: 100) | - | No |
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

**Usage:** `skills update <slug> [options]`

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

**Usage:** `skills delete <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | Delete a global skill | - | No |
| `--project <slug>` | Delete a project-scoped skill | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `skills import`

Import a skill from a public GitHub repository (skills.sh-compatible). Source: <owner>/<repo> or <owner>/<repo>/<skill-name>

**Usage:** `skills import <source> [options]`

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

**Usage:** `mcp-servers update <slug> [options]`

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

**Usage:** `mcp-servers delete <slug> [options]`

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

**Usage:** `mcp add [client] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-g, --groups <groups>` | Legacy tool-group filter (optional; does not restrict temps/temps_write permissions) | - | No |
| `-w, --write` | Enable write tools (deploy, delete, restart, etc). Default: read-only | - | No |
| `-k, --api-key <key>` | Use this API key instead of creating or prompting for one | - | No |
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |
| `-y, --yes` | Skip prompts and confirmation (uses defaults; requires --api-key or an existing login) | - | No |
| `--api-key-file <path>` | Read the API key from a file (- reads stdin) instead of --api-key | - | No |
| `--api-key-stdin` | Read the API key from stdin instead of --api-key | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `mcp remove`

Remove the Temps MCP server from an AI client

**Usage:** `mcp remove [client]`

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
- `create` (`add`) - Create or update a secret (upsert by name). Prefer --value-file or --value-stdin, which keep the value out of shell history
- `update` - Update an existing secret (alias for create — upserts). The value must be re-supplied: the server never returns it
- `delete` (`rm`) - Delete a secret

### `secrets list` (alias: `ls`)

List all secrets (values are masked)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `secrets create` (alias: `add`)

Create or update a secret (upsert by name). Prefer --value-file or --value-stdin, which keep the value out of shell history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Secret name | - | Yes |
| `-v, --value <value>` | Secret value on the command line (visible in shell history and `ps`). `-v @path` reading a file is deprecated: use --value-file | - | No |
| `-t, --type <type>` | Secret type: "env" (default) or "file" | `env` | No |
| `-m, --mount-path <path>` | Absolute path inside sandbox where file-type secret is written (required for --type file) | - | No |
| `-d, --description <description>` | Human-readable description | - | No |
| `-p, --project <slug-or-id>` | Scope the secret to one project (default: instance-wide) | - | No |
| `--value-file <path>` | Read the secret value from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the secret value from stdin, e.g. piped from a secret manager | - | No |
| `--literal` | Send a -v value that starts with @ as is, instead of reading it as a file path (deprecated form) | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `secrets update`

Update an existing secret (alias for create — upserts). The value must be re-supplied: the server never returns it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Secret name | - | Yes |
| `-v, --value <value>` | New value on the command line (visible in shell history). `-v @path` is deprecated: use --value-file | - | No |
| `-t, --type <type>` | Secret type: "env" or "file" | - | No |
| `-m, --mount-path <path>` | New mount path (file type only) | - | No |
| `-d, --description <description>` | New description | - | No |
| `-p, --project <slug-or-id>` | The project the secret is scoped to (default: instance-wide) | - | No |
| `--value-file <path>` | Read the new secret value from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the new secret value from stdin, e.g. piped from a secret manager | - | No |
| `--literal` | Send a -v value that starts with @ as is, instead of reading it as a file path (deprecated form) | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `secrets delete` (alias: `rm`)

Delete a secret

**Usage:** `secrets delete <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Delete the secret of this project (default: the instance-wide one) | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

## `sandbox`

Manage standalone sandboxes (/v1/sandbox API)

**Subcommands:**

- `create` - Create a new sandbox
- `list` (`ls`) - List your sandboxes
- `nodes` - List nodes that can run sandboxes and control which ones take new sandboxes
- `show` - Show a sandbox: status, image, node, expiry, preview access and outbound policy
- `rm` (`stop`, `destroy`) - Remove a sandbox permanently (aliases: stop, destroy)
- `pause` - Pause a running sandbox (non-destructive — resume later with `sandbox resume`)
- `resume` - Resume a paused sandbox
- `restart` - Restart a running sandbox (preserves filesystem)
- `clone` - Clone a git repo or extract a tarball into a running sandbox
- `shell` (`attach`) - Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab
- `extend` - Extend a sandbox's idle timeout
- `exec` - Run a command inside a sandbox, print its output and exit with its exit code. Put `--` before the command: `exec ID -- ls -la`
- `logs` - Print the output of a job started with `exec --detach`, following it until the job ends
- `domain` - Resolve the preview URL for a port inside a sandbox
- `password` - Generate, rotate, or clear the preview-URL password for a sandbox
- `preview-access` - Show or set who may open the preview URLs: Temps login (default), the preview password, or anyone, optionally from listed networks only
- `egress` - Show or set where the sandbox may connect to: the internet, nothing, or only listed hosts and CIDRs. Private, loopback and metadata addresses are always refused
- `fs` - Filesystem operations inside a sandbox
- `snapshots` - Manage sandbox snapshots (ADR-037)
- `snapshot` - Take a snapshot of a sandbox
- `service-access` - Service access commands
- `internal-access` - Internal access commands
- `services` - Services commands
- `get-creation` - Read a create's steps by the 'creation_id' its 'POST /v1/sandboxes' sent
- `get-image-preparation` - Every host that can run sandboxes, with whether its default sandbox image is pulled (with download..
- `prepare-images` - Pull the default sandbox image on every eligible host (or one), so the first sandbox there starts without..
- `update-host-image-preparation` - Turn keeping one host's sandbox image prepared on or off
- `get-resource-defaults` - Effective resource defaults and input ceilings for the requested placement
- `rootfs-report` - Inspect rootfs storage: the Firecracker digest-keyed cache (with which sandboxes reference each entry) and..
- `rootfs-gc` - Reclaim rootfs cache entries not backing any live sandbox
- `get-cmd` - Get cmd
- `cmd-logs` - Stream a command's stdout/stderr as 'application/x-ndjson' ('@vercel/sandbox'-compatible)
- `cmd` - Run a command the '@vercel/sandbox' way: without --wait it returns the started command's id at once (read its output with cmd-logs); with --wait it prints the running and finished envelopes as JSON lines. For an interactive run use `temps sandbox exec`
- `get-creation-by` - Read the steps of the create that made a sandbox, while this server still tracks it (it is running, or..
- `list-dir` - List dir
- `write-files` - Batch-write multiple files in a single request
- `list-jobs` - List jobs
- `job-status` - Job status
- `kill-job` - Terminate a detached job
- `create-preview-link` - Mint a shareable link to a sandbox preview
- `resize` - Grow a Firecracker sandbox's root disk
- `issue-runtime-environment` - Issue the same scoped service variables used by a deployment runtime for the project attached to this sandbox
- `cmd-kill` - Kill a running command ('@vercel/sandbox'-compatible)

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
| `--pids-limit <n>` | Maximum number of processes (default 512) | - | No |
| `--creation-id <id>` | Your own id for this create (8-64 letters, digits, - or _), to follow its steps with `sandbox get-creation` | - | No |
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
| `--egress-allow <host\|cidr[:ports]>` | Restrict outbound traffic to this destination (repeatable; no ports = 80 and 443). IPv6 with ports: [addr/prefix]:port | - | No |
| `--egress-deny-all` | Allow no outbound traffic at all | - | No |
| `--from-snapshot <snap-id>` | Create sandbox from a snapshot (mutually exclusive with --image) | - | No |
| `--node <name\|id>` | Node to run the sandbox on (name, id, or 'control-plane'). Omit to let Temps place it; see `sandbox nodes` | - | No |
| `--backend <backend>` | Isolation backend: docker, firecracker, microsandbox. Omit for the host default. An unavailable backend fails instead of downgrading | - | No |
| `--allow-docker` | Allow installing/running Docker inside microsandbox; relaxes guest privilege restrictions. Requires --backend microsandbox | - | No |
| `--json` | Output as JSON | - | No |
| `--git-password-file <path>` | Read the Git HTTP password or token from a file (- reads stdin) instead of --git-password | - | No |
| `--git-password-stdin` | Read the Git HTTP password or token from stdin instead of --git-password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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

### `sandbox nodes`

List nodes that can run sandboxes and control which ones take new sandboxes

**Subcommands:**

- `list` - List nodes with their sandbox placement state (the default)
- `show` - Show a node's placement state and every live sandbox on it, from all users (name, id, or 'control-plane'). Admin only
- `set` - Allow exactly these nodes to run new sandboxes, replacing the list (names, ids, or 'control-plane'). Admin only
- `allow` - Add nodes to the list allowed to run new sandboxes. Admin only
- `deny` - Stop nodes from taking new sandboxes (existing ones keep running). Admin only
- `allow-all` - Allow every node, including the control plane, to run sandboxes (the default). Admin only
- `deny-all` - Stop every node from taking new sandboxes (existing ones keep running). Admin only
- `evict` - Destroy every sandbox on a worker node, from all users, so the node can be removed. Works on a node that is offline for good. Admin only

#### `sandbox nodes list`

List nodes with their sandbox placement state (the default)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes show`

Show a node's placement state and every live sandbox on it, from all users (name, id, or 'control-plane'). Admin only

**Usage:** `sandbox nodes show <node> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default 1) | - | No |
| `--page-size <n>` | Items per page (default 20, max 100) | - | No |
| `--json` | Output as JSON | - | No |

#### `sandbox nodes set`

Allow exactly these nodes to run new sandboxes, replacing the list (names, ids, or 'control-plane'). Admin only

**Usage:** `sandbox nodes set <nodes...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes allow`

Add nodes to the list allowed to run new sandboxes. Admin only

**Usage:** `sandbox nodes allow <nodes...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes deny`

Stop nodes from taking new sandboxes (existing ones keep running). Admin only

**Usage:** `sandbox nodes deny <nodes...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes allow-all`

Allow every node, including the control plane, to run sandboxes (the default). Admin only

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes deny-all`

Stop every node from taking new sandboxes (existing ones keep running). Admin only

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes evict`

Destroy every sandbox on a worker node, from all users, so the node can be removed. Works on a node that is offline for good. Admin only

**Usage:** `sandbox nodes evict <node> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation prompt | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox show`

Show a sandbox: status, image, node, expiry, preview access and outbound policy

**Usage:** `sandbox show <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

### `sandbox rm` (alias: `stop`, `destroy`)

Remove a sandbox permanently (aliases: stop, destroy)

**Usage:** `sandbox rm <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation prompt | - | No |

### `sandbox pause`

Pause a running sandbox (non-destructive — resume later with `sandbox resume`)

**Usage:** `sandbox pause <id>`

### `sandbox resume`

Resume a paused sandbox

**Usage:** `sandbox resume <id>`

### `sandbox restart`

Restart a running sandbox (preserves filesystem)

**Usage:** `sandbox restart <id>`

### `sandbox clone`

Clone a git repo or extract a tarball into a running sandbox

**Usage:** `sandbox clone <id> [options]`

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
| `--strip-git-metadata` | Remove the .git directory after cloning | - | No |
| `--git-password-file <path>` | Read the Git HTTP password or token from a file (- reads stdin) instead of --git-password | - | No |
| `--git-password-stdin` | Read the Git HTTP password or token from stdin instead of --git-password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `sandbox shell` (alias: `attach`)

Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab

**Usage:** `sandbox shell <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--tab <name>` | Tab to attach to; reusing a name reattaches to the program already running in it | `main` | No |
| `--cmd <command>` | Program to start when the tab is created, e.g. "claude" (default: login shell) | - | No |

### `sandbox extend`

Extend a sandbox's idle timeout

**Usage:** `sandbox extend <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--secs <seconds>` | Extra seconds to add to the current expiry | - | Yes |

### `sandbox exec`

Run a command inside a sandbox, print its output and exit with its exit code. Put `--` before the command: `exec ID -- ls -la`

**Usage:** `sandbox exec <id> [args...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--detach` | Start in background and print a job ID instead of waiting | - | No |
| `--cwd <path>` | Working directory inside the sandbox | - | No |
| `-e, --env <KEY=VAL>` | Env var for this exec (repeatable) | - | No |

### `sandbox logs`

Print the output of a job started with `exec --detach`, following it until the job ends

**Usage:** `sandbox logs <id> <jobId>`

### `sandbox domain`

Resolve the preview URL for a port inside a sandbox

**Usage:** `sandbox domain <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--port <port>` | Port inside the sandbox (1..=65535) | - | Yes |

### `sandbox password`

Generate, rotate, or clear the preview-URL password for a sandbox

**Usage:** `sandbox password <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--rotate` | Generate a new random password and set it (default when no flag is given) | - | No |
| `--length <n>` | Length of the generated password (8..=256, default 24) | - | No |
| `--clear` | Remove the preview password — preview URLs become open again | - | No |

### `sandbox preview-access`

Show or set who may open the preview URLs: Temps login (default), the preview password, or anyone, optionally from listed networks only

**Usage:** `sandbox preview-access <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--access <mode>` | temps_login (alias 'login'), password, or public. 'password' needs a preview password first (`sandbox password`) | - | No |
| `--allow-ip <ip\|cidr>` | Only these client networks may open previews, in every mode (repeatable; replaces the list) | - | No |
| `--any-ip` | Clear the allow-list: any client address may open previews | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox egress`

Show or set where the sandbox may connect to: the internet, nothing, or only listed hosts and CIDRs. Private, loopback and metadata addresses are always refused

**Usage:** `sandbox egress <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--internet` | Remove the restriction | - | No |
| `--deny-all` | Allow no outbound traffic at all | - | No |
| `--allow <host\|cidr[:ports]>` | Allow only these destinations (repeatable; replaces the policy; no ports = 80 and 443). IPv6 with ports: [addr/prefix]:port | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox fs`

Filesystem operations inside a sandbox

**Subcommands:**

- `read` - Read a file from the sandbox
- `write` - Write a file to the sandbox
- `stat` - Stat a path inside the sandbox
- `mkdir` - Create a directory inside the sandbox (mkdir -p)

#### `sandbox fs read`

Read a file from the sandbox

**Usage:** `sandbox fs read <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute file path inside the sandbox | - | Yes |
| `--out <localPath>` | Write to this local file (stdout when omitted) | - | No |

#### `sandbox fs write`

Write a file to the sandbox

**Usage:** `sandbox fs write <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute target path inside the sandbox | - | Yes |
| `--file <localPath>` | Local source file to upload (mutually exclusive with --content) | - | No |
| `--content <string>` | Inline string content to write | - | No |
| `--mode <octal>` | Unix permission mask (default: 0644) | - | No |

#### `sandbox fs stat`

Stat a path inside the sandbox

**Usage:** `sandbox fs stat <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute path inside the sandbox | - | Yes |
| `--json` | Output as JSON | - | No |

#### `sandbox fs mkdir`

Create a directory inside the sandbox (mkdir -p)

**Usage:** `sandbox fs mkdir <id> [options]`

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

**Usage:** `sandbox snapshots show <snapId> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox snapshots delete` (alias: `rm`)

Delete a snapshot permanently

**Usage:** `sandbox snapshots delete <snapId> [options]`

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

**Usage:** `sandbox snapshot <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--label <label>` | Human-readable label for the snapshot | - | No |
| `--wait` | Wait until the snapshot reaches ready or failed status | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox service-access`

Service access commands

**Subcommands:**

- `list` - List access services
- `create` - Create access service
- `get` - Get access service
- `update` - Update access service
- `list-grants` - List service grants
- `request-grant` - Request service grant
- `approve-grant` - Approve service grant
- `revoke-grant` - Revoke service grant
- `capability` - Access capability

#### `sandbox service-access list`

List access services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access create`

Create access service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--clear-credential` | Body field clear_credential | - | No |
| `--no-clear-credential` | Set clear_credential to false | - | No |
| `--credential-stdin` | A credential (read from stdin) | - | No |
| `--credential-file <path>` | A credential (read from a file) | - | No |
| `--enabled` | Allows requests to be saved; does not enable a network connection [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--expected-version <n>` | Required for updates; omit for creation | - | No |
| `--max-ttl-seconds <n>` | Maximum lifetime of a request, between 60 and 86400 seconds [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--policy <json>` | Exact destination and request scope (JSON) [required] | - | No |
| `--vault-id <value>` | Add a newly created service to this vault in the same transaction | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access get`

Get access service

**Usage:** `sandbox service-access get <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access update`

Update access service

**Usage:** `sandbox service-access update <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--clear-credential` | Body field clear_credential | - | No |
| `--no-clear-credential` | Set clear_credential to false | - | No |
| `--credential-stdin` | A credential (read from stdin) | - | No |
| `--credential-file <path>` | A credential (read from a file) | - | No |
| `--enabled` | Allows requests to be saved; does not enable a network connection [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--expected-version <n>` | Required for updates; omit for creation | - | No |
| `--max-ttl-seconds <n>` | Maximum lifetime of a request, between 60 and 86400 seconds [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--policy <json>` | Exact destination and request scope (JSON) [required] | - | No |
| `--vault-id <value>` | Add a newly created service to this vault in the same transaction | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access list-grants`

List service grants

**Usage:** `sandbox service-access list-grants <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access request-grant`

Request service grant

**Usage:** `sandbox service-access request-grant <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--service-id <value>` | Body field service_id [required] | - | No |
| `--service-version <n>` | Body field service_version [required] | - | No |
| `--ttl-seconds <n>` | Absolute expiry is calculated at creation, even if the broker is unavailable [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access approve-grant`

Approve service grant

**Usage:** `sandbox service-access approve-grant <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--request-id <value>` | Body field request_id [required] | - | No |
| `--service-id <value>` | Body field service_id [required] | - | No |
| `--service-version <n>` | Body field service_version [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access revoke-grant`

Revoke service grant

**Usage:** `sandbox service-access revoke-grant <sandbox-id> <grant-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access capability`

Access capability

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox internal-access`

Internal access commands

**Subcommands:**

- `list-grants` - List the projects allowed to call this project's internal addresses
- `create-grant` - Allow another project to call this project's internal addresses
- `list-internal-access-sandbox-grants` - List the sandboxes allowed to call this project's internal addresses
- `create-internal-access-sandbox-grant` - Allow one of your sandboxes to call this project's internal addresses
- `delete-grant` - Stop a sandbox from calling this project's internal addresses
- `delete-internal-access-grant` - Stop another project from calling this project's internal addresses
- `list-addresses` - List the internal addresses one of your sandboxes may call

#### `sandbox internal-access list-grants`

List the projects allowed to call this project's internal addresses

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Only grants that cover this environment of the project: its own and the project-wide ones | - | No |
| `--page <n>` | 1-based page, default 1 | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access create-grant`

Allow another project to call this project's internal addresses

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--caller-environment-id <n>` | Only this environment of the calling project; omit for every environment, present and future | - | No |
| `--caller-project-id <n>` | The project whose workloads may call [required] | - | No |
| `--description <value>` | Why the grant exists, up to 200 characters | - | No |
| `--target-environment-id <n>` | The environment of this project that may be called; omit for every environment, present and future | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access list-internal-access-sandbox-grants`

List the sandboxes allowed to call this project's internal addresses

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Only grants that cover this environment of the project: its own and the project-wide ones | - | No |
| `--page <n>` | 1-based page, default 1 | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access create-internal-access-sandbox-grant`

Allow one of your sandboxes to call this project's internal addresses

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--description <value>` | Why the grant exists, up to 200 characters | - | No |
| `--sandbox-id <value>` | Public id of one of your sandboxes [required] | - | No |
| `--target-environment-id <n>` | The environment of this project that may be called; omit for every environment, present and future | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access delete-grant`

Stop a sandbox from calling this project's internal addresses

**Usage:** `sandbox internal-access delete-grant <grant-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access delete-internal-access-grant`

Stop another project from calling this project's internal addresses

**Usage:** `sandbox internal-access delete-internal-access-grant <grant-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access list-addresses`

List the internal addresses one of your sandboxes may call

**Usage:** `sandbox internal-access list-addresses <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | 1-based page, default 1 | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox services`

Services commands

**Subcommands:**

- `list` - List sandbox services
- `request` - Request sandbox service
- `approve` - Approve sandbox service
- `revoke` - Revoke sandbox service

#### `sandbox services list`

List sandbox services

**Usage:** `sandbox services list <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox services request`

Request sandbox service

**Usage:** `sandbox services request <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--service-id <value>` | Body field service_id [required] | - | No |
| `--service-version <n>` | Body field service_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox services approve`

Approve sandbox service

**Usage:** `sandbox services approve <sandbox-id> <access-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version [required] | - | No |
| `--expires-at <value>` | Omit for the sandbox lifetime | - | No |
| `--service-version <n>` | Body field service_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox services revoke`

Revoke sandbox service

**Usage:** `sandbox services revoke <sandbox-id> <access-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox get-creation`

Read a create's steps by the 'creation_id' its 'POST /v1/sandboxes' sent

**Usage:** `sandbox get-creation <creation-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox get-image-preparation`

Every host that can run sandboxes, with whether its default sandbox image is pulled (with download..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox prepare-images`

Pull the default sandbox image on every eligible host (or one), so the first sandbox there starts without..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node-id <n>` | The host to prepare: a node id, or '0' for the control plane | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox update-host-image-preparation`

Turn keeping one host's sandbox image prepared on or off

**Usage:** `sandbox update-host-image-preparation <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--keep-prepared` | Keep this host's default sandbox image pulled, re-pulling it when an upgrade changes the default. 'false'... [required] | - | No |
| `--no-keep-prepared` | Set keep_prepared to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox get-resource-defaults`

Effective resource defaults and input ceilings for the requested placement

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <value>` | Query parameter node | - | No |
| `--backend <value>` | Which isolation backend a sandbox runs on (ADR-029, ADR-050) (one of: docker, firecracker, microsandbox, local) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox rootfs-report`

Inspect rootfs storage: the Firecracker digest-keyed cache (with which sandboxes reference each entry) and..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox rootfs-gc`

Reclaim rootfs cache entries not backing any live sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox get-cmd`

Get cmd

**Usage:** `sandbox get-cmd <id> <cmd-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox cmd-logs`

Stream a command's stdout/stderr as 'application/x-ndjson' ('@vercel/sandbox'-compatible)

**Usage:** `sandbox cmd-logs <id> <cmd-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

### `sandbox cmd`

Run a command the '@vercel/sandbox' way: without --wait it returns the started command's id at once (read its output with cmd-logs); with --wait it prints the running and finished envelopes as JSON lines. For an interactive run use `temps sandbox exec`

**Usage:** `sandbox cmd <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--command <value>` | Program to run [required] | - | No |
| `--args <value>` | Argument (repeatable, in order) (repeatable) | - | No |
| `--cwd <value>` | Working directory inside the sandbox | - | No |
| `--env <json>` | Environment variables as a JSON object, e.g. {"NODE_ENV":"test"} (JSON) | - | No |
| `--sudo` | Run the command privileged | - | No |
| `--no-sudo` | Set sudo to false | - | No |
| `--wait` | Wait for the command to finish | - | No |
| `--no-wait` | Set wait to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

### `sandbox get-creation-by`

Read the steps of the create that made a sandbox, while this server still tracks it (it is running, or..

**Usage:** `sandbox get-creation-by <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox list-dir`

List dir

**Usage:** `sandbox list-dir <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Absolute path of a directory inside the sandbox [required] | - | No |
| `--page <n>` | 1-based page (default 1) | - | No |
| `--page-size <n>` | Entries per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox write-files`

Batch-write multiple files in a single request

**Usage:** `sandbox write-files <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--files <json>` | List of files to write (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox list-jobs`

List jobs

**Usage:** `sandbox list-jobs <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox job-status`

Job status

**Usage:** `sandbox job-status <id> <job-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox kill-job`

Terminate a detached job

**Usage:** `sandbox kill-job <id> <job-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--force` | When true, sends SIGKILL immediately | - | No |
| `--no-force` | Set force to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox create-preview-link`

Mint a shareable link to a sandbox preview

**Usage:** `sandbox create-preview-link <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Path the recipient lands on | - | No |
| `--port <n>` | Port inside the sandbox the preview serves on [required] | - | No |
| `--ttl-seconds <n>` | How long the link stays usable, in seconds | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox resize`

Grow a Firecracker sandbox's root disk

**Usage:** `sandbox resize <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--disk-size-mb <n>` | New root disk size in MB [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox issue-runtime-environment`

Issue the same scoped service variables used by a deployment runtime for the project attached to this sandbox

**Usage:** `sandbox issue-runtime-environment <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox cmd-kill`

Kill a running command ('@vercel/sandbox'-compatible)

**Usage:** `sandbox cmd-kill <id> <cmd-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--force` | Optional: when true, SIGKILL instead of SIGTERM | - | No |
| `--no-force` | Set force to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

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

**Usage:** `workflow run [slug] [options]`

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

## `workflows`

Durable application workflows

**Subcommands:**

- `list` - List detected workflows and onboarding status
- `runs` - List a bounded page of runs
- `get` - Get run metadata; payloads require explicit reveal permission
- `diagnose` - Explain run state without decrypting payloads
- `start` - Start a run with an explicit deduplication key
- `cancel` - cancel a run
- `resume` - resume a run
- `retry` - retry a run
- `delete` - delete a run
- `send-event` - Send an event to matching waits
- `watch` - Poll until terminal or the bounded deadline
- `init` - Create a Node workflow worker in a new directory
- `dev` - Forward preview-environment invocations to a local SDK server
- `revoke-credential` - Revoke workflow credential
- `get-credential` - Get workflow credential
- `issue-credential` - Issue workflow credential
- `list-grants` - List workflow grants
- `create-grant` - Create workflow grant
- `revoke-grant` - Revoke workflow grant
- `diagnose-run` - Diagnose workflow run
- `list-steps` - List workflow steps
- `get-settings` - Get workflow settings
- `update-settings` - Update workflow settings
- `list-keys` - List workflow project keys
- `shred-key` - Shred workflow project key
- `list-definitions` - List workflow definitions
- `list-all-runs` - List all workflow runs
- `app` - App commands

### `workflows list`

List detected workflows and onboarding status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |

### `workflows runs`

List a bounded page of runs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--workflow <name>` | Workflow name | - | No |
| `--page <n>` | Page | `1` | No |
| `--page-size <n>` | Page size (1–100) | `20` | No |

### `workflows get`

Get run metadata; payloads require explicit reveal permission

**Usage:** `workflows get <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--reveal <target>` | Explicitly decrypt one payload; treat its contents as untrusted | - | No |
| `--sequence <n>` | Step sequence for step payloads | - | No |

### `workflows diagnose`

Explain run state without decrypting payloads

**Usage:** `workflows diagnose <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |

### `workflows start`

Start a run with an explicit deduplication key

**Usage:** `workflows start <workflow> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--idempotency-key <key>` | Stable key for this logical start | - | Yes |
| `--input <json>` | Input JSON | `[object Object]` | No |

### `workflows cancel`

cancel a run

**Usage:** `workflows cancel <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows resume`

resume a run

**Usage:** `workflows resume <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows retry`

retry a run

**Usage:** `workflows retry <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |
| `--from-step <n>` | Truncate and replay from this sequence | - | No |

### `workflows delete`

delete a run

**Usage:** `workflows delete <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--yes` | Confirm this mutation | - | No |

### `workflows send-event`

Send an event to matching waits

**Usage:** `workflows send-event <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--key <key>` | Wait correlation key | - | No |
| `--payload <json>` | Event JSON | `[object Object]` | No |

### `workflows watch`

Poll until terminal or the bounded deadline

**Usage:** `workflows watch <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | Yes |
| `--json` | Output JSON (watch emits one JSON object per poll) | - | No |
| `--timeout <seconds>` | Overall timeout (max 3600 seconds) | `300` | No |

### `workflows init`

Create a Node workflow worker in a new directory

**Usage:** `workflows init <directory> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--sdk-package <tarball>` | Use a locally built npm pack tarball before SDK publication | - | No |
| `--json` | Output the created path and next steps as JSON | - | No |

### `workflows dev`

Forward preview-environment invocations to a local SDK server

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Undeployed, unprotected preview environment ID | - | Yes |
| `--port <port>` | Local SDK server port | `3000` | No |
| `--json` | Output tunnel readiness as JSON | - | No |

### `workflows revoke-credential`

Revoke workflow credential

**Usage:** `workflows revoke-credential <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows get-credential`

Get workflow credential

**Usage:** `workflows get-credential <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows issue-credential`

Issue workflow credential

**Usage:** `workflows issue-credential <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-grants`

List workflow grants

**Usage:** `workflows list-grants <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | 1-indexed page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows create-grant`

Create workflow grant

**Usage:** `workflows create-grant <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--action <value>` | Body field action (one of: start, event) [required] | - | No |
| `--resource <value>` | Body field resource [required] | - | No |
| `--source-environment-id <n>` | Body field source_environment_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows revoke-grant`

Revoke workflow grant

**Usage:** `workflows revoke-grant <environment-id> <grant-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows diagnose-run`

Diagnose workflow run

**Usage:** `workflows diagnose-run <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-steps`

List workflow steps

**Usage:** `workflows list-steps <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | 1-indexed page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows get-settings`

Get workflow settings

**Usage:** `workflows get-settings <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows update-settings`

Update workflow settings

**Usage:** `workflows update-settings <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--drain-timeout-secs <n>` | Opt-in deploy drain: pause new invocations and wait for running attempts | - | No |
| `--host-service <value>` | Compose service hosting the workflow handler; 'None': the main service | - | No |
| `--inline-payload-limit-bytes <n>` | Body field inline_payload_limit_bytes [required] | - | No |
| `--invocation-deadline-secs <n>` | Body field invocation_deadline_secs [required] | - | No |
| `--lease-secs <n>` | Body field lease_secs [required] | - | No |
| `--managed-store-service-id <n>` | Managed PostgreSQL journal for new runs; existing runs keep their store | - | No |
| `--max-active-runs <n>` | Body field max_active_runs [required] | - | No |
| `--max-concurrent-runs <n>` | Body field max_concurrent_runs [required] | - | No |
| `--max-journal-bytes-per-run <n>` | Body field max_journal_bytes_per_run [required] | - | No |
| `--max-payload-bytes <n>` | Body field max_payload_bytes [required] | - | No |
| `--max-steps-per-run <n>` | Body field max_steps_per_run [required] | - | No |
| `--retention-days <n>` | Body field retention_days [required] | - | No |
| `--stuck-after-failures <n>` | Body field stuck_after_failures [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-keys`

List workflow project keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | 1-indexed page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows shred-key`

Shred workflow project key

**Usage:** `workflows shred-key <key-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--confirm-key-id <value>` | Exact key UUID, confirming that its retained payloads become unreadable [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-definitions`

List workflow definitions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | 1-based page; default 1 | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows list-all-runs`

List all workflow runs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Where a run is in its lifecycle (ADR-106 §3) (one of: queued, running, sleeping, waiting, retrying, completed, failed, cancelled, stuck) | - | No |
| `--workflow <value>` | Query parameter workflow | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--cursor <value>` | Query parameter cursor | - | No |
| `--json` | Output in JSON format | - | No |

### `workflows app`

App commands

**Subcommands:**

- `send-event` - App send workflow event
- `send-granted-event` - App send granted event
- `start-granted` - App start granted workflow
- `start-run` - App start workflow run
- `get-run` - App get workflow run

#### `workflows app send-event`

App send workflow event

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--key <value>` | Narrows delivery to runs waiting on this key | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--payload <json>` | Body field payload (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `workflows app send-granted-event`

App send granted event

**Usage:** `workflows app send-granted-event <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--key <value>` | Narrows delivery to runs waiting on this key | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--payload <json>` | Body field payload (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `workflows app start-granted`

App start granted workflow

**Usage:** `workflows app start-granted <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--idempotency-key <value>` | A second start with the same key returns the first run | - | No |
| `--body-input <json>` | Body field input (JSON) | - | No |
| `--workflow <value>` | Body field workflow [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `workflows app start-run`

App start workflow run

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--idempotency-key <value>` | A second start with the same key returns the first run | - | No |
| `--body-input <json>` | Body field input (JSON) | - | No |
| `--workflow <value>` | Body field workflow [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `workflows app get-run`

App get workflow run

**Usage:** `workflows app get-run <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `revenue`

Manage revenue integrations and import historical data

**Subcommands:**

- `import` - Import historical revenue data from a CSV export
- `recent-events` - Recent ingested events for the activity feed
- `list-integrations` - List revenue integrations for a project
- `create-integration` - Create a new revenue integration
- `delete-integration` - Delete a revenue integration (permanent — use rotate_token to refresh credentials without breaking history)
- `update-config` - Replace the typed provider config on an integration
- `import-invoices-csv` - Import a Stripe invoices CSV export
- `import-subscriptions-csv` - Import a Stripe subscriptions CSV export
- `rotate-token` - Rotate the webhook path token
- `update-secret` - Replace the stored signing secret without rotating the webhook URL
- `metrics-customers` - New + churned customers per bucket
- `metrics-mrr` - Bucketed MRR timeseries for the revenue chart
- `metrics-summary` - Current MRR / ARR / churn / ARPU for a project, in one currency
- `global-events` - Org-wide revenue events across every project
- `metrics-global-mrr` - Org-wide MRR total, summed across every project in the install
- `metrics-global-summary` - Org-wide revenue summary: MRR, paid cash (30d + all-time), refunds, active subscriptions/customers, and..
- `list-providers` - List registered providers (what the UI needs to render the "Connect" dropdown + its wizard instructions)

### `revenue import`

Import historical revenue data from a CSV export

**Subcommands:**

- `subscriptions` - Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)
- `invoices` - Import paid invoices CSV to backfill the revenue chart

#### `revenue import subscriptions`

Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)

**Usage:** `revenue import subscriptions <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (defaults to linked project) | - | No |
| `--integration-id <id>` | Target integration ID (auto-detected if only one exists) | - | No |
| `--provider <name>` | Target provider name (e.g., stripe) | - | No |
| `--json` | Output the import outcome as JSON (suppresses spinners) | - | No |

#### `revenue import invoices`

Import paid invoices CSV to backfill the revenue chart

**Usage:** `revenue import invoices <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (defaults to linked project) | - | No |
| `--integration-id <id>` | Target integration ID (auto-detected if only one exists) | - | No |
| `--provider <name>` | Target provider name (e.g., stripe) | - | No |
| `--json` | Output the import outcome as JSON (suppresses spinners) | - | No |

### `revenue recent-events`

Recent ingested events for the activity feed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue list-integrations`

List revenue integrations for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue create-integration`

Create a new revenue integration

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--provider <value>` | Registered provider name, e.g. "stripe" [required] | - | No |
| `--signing-secret-stdin` | Signing secret from the provider's dashboard [required] (read from stdin) | - | No |
| `--signing-secret-file <path>` | Signing secret from the provider's dashboard [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue delete-integration`

Delete a revenue integration (permanent — use rotate_token to refresh credentials without breaking history)

**Usage:** `revenue delete-integration <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue update-config`

Replace the typed provider config on an integration

**Usage:** `revenue update-config <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Provider-specific integration settings persisted in 'revenue_integrations.config' (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue import-invoices-csv`

Import a Stripe invoices CSV export

**Usage:** `revenue import-invoices-csv <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue import-subscriptions-csv`

Import a Stripe subscriptions CSV export

**Usage:** `revenue import-subscriptions-csv <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue rotate-token`

Rotate the webhook path token

**Usage:** `revenue rotate-token <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue update-secret`

Replace the stored signing secret without rotating the webhook URL

**Usage:** `revenue update-secret <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--signing-secret-stdin` | New signing secret from the provider's dashboard [required] (read from stdin) | - | No |
| `--signing-secret-file <path>` | New signing secret from the provider's dashboard [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue metrics-customers`

New + churned customers per bucket

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue metrics-mrr`

Bucketed MRR timeseries for the revenue chart

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue metrics-summary`

Current MRR / ARR / churn / ARPU for a project, in one currency

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue global-events`

Org-wide revenue events across every project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter to a single project | - | No |
| `--from <value>` | Lower bound (inclusive), ISO-8601 | - | No |
| `--to <value>` | Upper bound (inclusive), ISO-8601 | - | No |
| `--event-types <value>` | Comma-separated event types (e.g. 'invoice.paid,charge.succeeded') | - | No |
| `--limit <n>` | Max rows, default 100, max 500 | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue metrics-global-mrr`

Org-wide MRR total, summed across every project in the install

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `revenue metrics-global-summary`

Org-wide revenue summary: MRR, paid cash (30d + all-time), refunds, active subscriptions/customers, and..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--currency <value>` | ISO-4217 currency code, default USD | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue list-providers`

List registered providers (what the UI needs to render the "Connect" dropdown + its wizard instructions)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

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

**Usage:** `session-replay visitor <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default: 1) | `1` | No |
| `--per-page <n>` | Sessions per page (default: 25) | `25` | No |
| `--json` | Output raw JSON | - | No |

### `session-replay show`

Show session metadata (use numeric session ID from list)

**Usage:** `session-replay show <visitor-id> <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output raw JSON | - | No |

### `session-replay events`

Download or page through all rrweb events for a session

**Usage:** `session-replay events <visitor-id> <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page of events to display (default: 1) | `1` | No |
| `--limit <n>` | Events per page (default: 50) | `50` | No |
| `--output <file>` | Write all events as JSON to a file (skips paged display) | - | No |
| `--json` | Print all events as JSON to stdout | - | No |

### `session-replay delete` (alias: `rm`)

Delete a session replay

**Usage:** `session-replay delete <visitor-id> <session-id> [options]`

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

**Usage:** `traefik-discovery routes enable <host> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery routes disable`

Stop routing one discovered host without touching the container labels; the route stays listed so you can see what was found

**Usage:** `traefik-discovery routes disable <host> [options]`

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

**Usage:** `traefik-discovery tls request <host> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--challenge-type <type>` | Challenge type: http-01 (default) or dns-01 | `http-01` | No |
| `--acknowledge-manual-dns-renewal` | Confirm you accept manual DNS renewal when no auto-manage DNS zone is configured | - | No |
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery tls revoke`

Remove TLS authorization for a discovered route. Stops Temps from attempting renewal. Does NOT delete the certificate — use `temps domains delete <host>` to remove the certificate itself.

**Usage:** `traefik-discovery tls revoke <host> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery tls import`

Import certificates from a Traefik acme.json file (Path B). Use this to get HTTPS immediately at cutover — Traefik already holds the cert, so there is no outage window. Each host is validated (8-step X.509 chain) and a per-host result is returned. Add --dry-run to preview without writing.

**Usage:** `traefik-discovery tls import <acme-json-file> [options]`

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

**Usage:** `init [project-slug] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Project name (for new projects) | - | No |
| `-y, --yes` | Skip confirmation prompts | - | No |

## `link`

Link current directory to a Temps project

**Usage:** `link [project-slug] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Set default environment | - | No |

## `up`

Deploy the current project (runs setup wizard if not linked)

**Usage:** `up [project] [options]`

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

**Usage:** `status [project] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-e, --environment <env>` | Filter by environment | - | No |
| `--json` | Output in JSON format | - | No |

## `ai`

AI assistant status for a project

**Subcommands:**

- `chat` - AI chat conversations: messages, attachments, sandboxes and workspaces
- `provider` - Manage the AI CLIs (Claude Code, Codex) Temps runs on this server
- `readiness` - Show which AI prerequisites this project meets, and how to fix the rest
- `apps` - Apps commands
- `coding-agents` - Coding agents commands
- `gateway` - Gateway commands
- `provider-status` - Provider status commands
- `workspace` - Workspace commands

### `ai chat`

AI chat conversations: messages, attachments, sandboxes and workspaces

**Subcommands:**

- `upload` - Attach a file to a conversation in chunks; resumable with --resume
- `create-assistant-conversation` - Start a chat with the Temps assistant
- `list-all-conversations` - List the current user's active conversations across all projects, most-recently-active first, annotated..
- `create-global-conversation` - Create a private user-owned operator thread without binding its lifetime or authority to a project
- `get-user-conversation` - Load one bounded history page for a private conversation by its owner-facing id
- `rename-user-conversation` - Rename user conversation
- `archive-user-conversation` - Archive user conversation
- `start-attachment-upload` - Start uploading a file in chunks, for clients behind a small request-size limit
- `cancel-attachment-upload` - Abandon an upload and free what it reserved
- `append-attachment-chunk` - Append the next chunk of an upload
- `complete-attachment-upload` - Finish an upload once every byte has arrived
- `upload-user-conversation-attachment` - Upload user conversation attachment
- `get-user-conversation-attachment` - Get user conversation attachment
- `compact-conversation` - Summarize the chat's older messages now
- `get-user-conversation-diagnostics` - Export the bounded, server-redacted diagnostic state Temps currently owns
- `list-goals` - List the goals a chat is watching or has finished with, newest first
- `cancel-goal` - Stop watching a goal
- `retry-goal` - Give a failed or waiting goal a fresh attempt budget and check it again now
- `send-user-message` - Submit a turn to a user-owned conversation
- `list-operations` - The deployments, backups and restores this chat started, newest first, with their current state
- `list-user-pending-actions` - List user pending actions
- `update-user-permission-mode` - Update user permission mode
- `resolve-user-permission` - Resolve user permission
- `list-resources` - The projects, databases and services, repositories and sandbox this chat made, with their current state
- `restore-user-conversation` - Restore a user-owned archived conversation
- `unlink-sandbox` - Unlink the chat's sandbox
- `get-sandbox` - Whether the chat can use a sandbox, and which one it is linked to
- `link-sandbox` - Link a sandbox to the chat, or create one for it
- `get-sandbox-changes` - The uncommitted Git changes of every repository under the sandbox work directory
- `get-sandbox-diff` - Both sides of one changed file: as last committed, and as it is now
- `track-sandbox-changes` - Put a folder of the sandbox under Git, so its changes show from now on: a '.gitignore' for dependencies,..
- `read-sandbox-file` - One file of the chat's sandbox, for the viewer
- `list-sandbox-files` - List one directory of the chat's sandbox: folders first, then files
- `connect-sandbox` - Let the chat's sandbox use a project's database
- `disconnect-sandbox` - Stop the chat's sandbox using a project's database
- `connect-sandbox-service` - Let the chat's sandbox use one service, whether or not it belongs to a project
- `disconnect-sandbox-service` - Stop the chat's sandbox using one service
- `wake-sandbox` - Start the chat's sandbox if it went to sleep, so the preview and the assistant's next command find it up
- `list-secret-requests` - List the credentials this chat has asked for and not yet used up or lost
- `provide-secret` - Provide the credential the assistant asked for
- `discard-secret` - Discard a credential request and anything already typed into it
- `stop-user-turn` - Stop user turn
- `get-suggestion-mode` - A chat's next-step suggestion setting, with the default it falls back to
- `update-suggestion-mode` - Set a chat's own next-step suggestion mode, or clear it to follow the user's default
- `suggest-next-steps` - Suggest a few next steps after the chat's latest answer
- `get-user-conversation-usage` - What this conversation has cost so far
- `leave-workspace` - Take this chat out of its workspace
- `get-workspace` - The workspace this chat is in, with its other chats
- `set-workspace` - Put this chat in a workspace, creating it when asked
- `get-user-pending-action` - Get user pending action
- `confirm-user-pending-action` - Confirm user pending action
- `reject-user-pending-action` - Reject user pending action
- `get-suggestion-default` - The user's default for next-step suggestions
- `update-suggestion-default` - Set the user's default for every chat that does not set its own mode
- `find-conversation` - Find the current user's existing chat for a context (returns 'null' if none yet)
- `create-conversation` - Get-or-create the current user's private chat for a context
- `list-conversations` - List the current user's active conversations for a project, most-recently-active first
- `get-conversation` - One bounded conversation-history page (excluding internal context rows)
- `rename-conversation` - Rename a conversation (set its human-facing title)
- `archive-conversation` - Archive (soft-delete) a conversation
- `send-message` - Submit a user message and start a server-owned turn
- `list-pending-actions` - List all pending actions for a conversation (most-recently-proposed first)
- `update-permission-mode` - Update permission mode
- `resolve-permission` - Resolve permission
- `stop-turn` - Explicitly cancel the server-owned active turn
- `get-pending-action` - Get a single pending action by its public id (scoped to the project)
- `confirm-pending-action` - Confirm a proposed AI action: validate permission, atomically claim, execute, persist outcome
- `reject-pending-action` - Reject a proposed AI action (no execution)

#### `ai chat upload`

Attach a file to a conversation in chunks; resumable with --resume

**Usage:** `ai chat upload <conversation-id> <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | File name to show (default: the file name) | - | No |
| `--mime-type <type>` | Content type (default: guessed from the extension) | - | No |
| `--resume <upload-id>` | Continue an interrupted upload instead of starting a new one | - | No |
| `--chunk-bytes <n>` | Bytes per chunk (default 32768, the server's limit) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat create-assistant-conversation`

Start a chat with the Temps assistant

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ai-model <value>` | Model id to start with; defaults to the provider's default model | - | No |
| `--ai-provider <value>` | An AI-gateway provider id ('gateway_key:<id>') | - | No |
| `--project-id <n>` | A project the chat is about | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-all-conversations`

List the current user's active conversations across all projects, most-recently-active first, annotated..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Conversation lifecycle state (defaults to active) (one of: active, archived) | - | No |
| `--scope <value>` | Limit results to the global workspace, or return every readable context (one of: all, global) | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of conversations per page (clamped to 1..=100) | - | No |
| `--search <value>` | Keep only chats whose title, or whose project's name or slug, contains this text (case-insensitive,... | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat create-global-conversation`

Create a private user-owned operator thread without binding its lifetime or authority to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ai-model <value>` | Body field ai_model | - | No |
| `--ai-permission-mode <value>` | Body field ai_permission_mode | - | No |
| `--ai-provider <value>` | A registered development harness | - | No |
| `--ai-thinking-level <value>` | Body field ai_thinking_level | - | No |
| `--kind <value>` | 'ai' (default) or 'terminal' | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-conversation`

Load one bounded history page for a private conversation by its owner-facing id

**Usage:** `ai chat get-user-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--before <value>` | Opaque next_before cursor returned by the previous page | - | No |
| `--limit <n>` | Messages per page (default 50, maximum 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat rename-user-conversation`

Rename user conversation

**Usage:** `ai chat rename-user-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--title <value>` | New human-facing title [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat archive-user-conversation`

Archive user conversation

**Usage:** `ai chat archive-user-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat start-attachment-upload`

Start uploading a file in chunks, for clients behind a small request-size limit

**Usage:** `ai chat start-attachment-upload <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mime-type <value>` | Content type the browser reported, if any | - | No |
| `--name <value>` | File name as the user knows it; unsafe characters are replaced [required] | - | No |
| `--size-bytes <n>` | Size of the whole file in bytes [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat cancel-attachment-upload`

Abandon an upload and free what it reserved

**Usage:** `ai chat cancel-attachment-upload <public-id> <upload-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat append-attachment-chunk`

Append the next chunk of an upload

**Usage:** `ai chat append-attachment-chunk <public-id> <upload-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--offset <n>` | Where this chunk starts in the file: the bytes received so far [required] | - | No |
| `--chunk <path>` | Body field chunk [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat complete-attachment-upload`

Finish an upload once every byte has arrived

**Usage:** `ai chat complete-attachment-upload <public-id> <upload-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat upload-user-conversation-attachment`

Upload user conversation attachment

**Usage:** `ai chat upload-user-conversation-attachment <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--file <path>` | Body field file [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-conversation-attachment`

Get user conversation attachment

**Usage:** `ai chat get-user-conversation-attachment <public-id> <attachment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <value>` | Query parameter name [required] | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat compact-conversation`

Summarize the chat's older messages now

**Usage:** `ai chat compact-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-conversation-diagnostics`

Export the bounded, server-redacted diagnostic state Temps currently owns

**Usage:** `ai chat get-user-conversation-diagnostics <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Persisted rows to include, newest window first (default/max 25) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-goals`

List the goals a chat is watching or has finished with, newest first

**Usage:** `ai chat list-goals <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat cancel-goal`

Stop watching a goal

**Usage:** `ai chat cancel-goal <public-id> <goal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat retry-goal`

Give a failed or waiting goal a fresh attempt budget and check it again now

**Usage:** `ai chat retry-goal <public-id> <goal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat send-user-message`

Submit a turn to a user-owned conversation

**Usage:** `ai chat send-user-message <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ai-model <value>` | Optional next-turn model, one advertised by the conversation's provider (or by 'ai_provider' when that is set) | - | No |
| `--ai-permission-mode <value>` | Optional next-turn permission mode for the pinned provider harness | - | No |
| `--ai-provider <value>` | Optional AI gateway provider ('gateway_key:<id>') for this and later turns | - | No |
| `--ai-thinking-level <value>` | Optional next-turn thinking level | - | No |
| `--attachments <json>` | Opaque references returned by the conversation attachment endpoint (JSON) | - | No |
| `--content <value>` | Body field content [required] | - | No |
| `--page-context <value>` | Optional, client-supplied description of the page/entity the user is currently viewing (e.g. a trace in a... | - | No |
| `--turn-id <value>` | Client-generated opaque idempotency key for this turn | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-operations`

The deployments, backups and restores this chat started, newest first, with their current state

**Usage:** `ai chat list-operations <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat list-user-pending-actions`

List user pending actions

**Usage:** `ai chat list-user-pending-actions <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat update-user-permission-mode`

Update user permission mode

**Usage:** `ai chat update-user-permission-mode <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--permission-mode <value>` | Provider permission mode to persist [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat resolve-user-permission`

Resolve user permission

**Usage:** `ai chat resolve-user-permission <public-id> <permission-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--decision <json>` | The user's decision for a pending permission request (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-resources`

The projects, databases and services, repositories and sandbox this chat made, with their current state

**Usage:** `ai chat list-resources <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat restore-user-conversation`

Restore a user-owned archived conversation

**Usage:** `ai chat restore-user-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat unlink-sandbox`

Unlink the chat's sandbox

**Usage:** `ai chat unlink-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-sandbox`

Whether the chat can use a sandbox, and which one it is linked to

**Usage:** `ai chat get-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat link-sandbox`

Link a sandbox to the chat, or create one for it

**Usage:** `ai chat link-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--create` | Create a fresh sandbox for this chat instead | - | No |
| `--no-create` | Set create to false | - | No |
| `--sandbox-id <value>` | Public id of one of the caller's own sandboxes | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-sandbox-changes`

The uncommitted Git changes of every repository under the sandbox work directory

**Usage:** `ai chat get-sandbox-changes <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat get-sandbox-diff`

Both sides of one changed file: as last committed, and as it is now

**Usage:** `ai chat get-sandbox-diff <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--repo <value>` | The repository root, absolute or relative to the work directory [required] | - | No |
| `--path <value>` | The file, relative to the repository root [required] | - | No |
| `--old-path <value>` | The path before a rename, relative to the repository root | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat track-sandbox-changes`

Put a folder of the sandbox under Git, so its changes show from now on: a '.gitignore' for dependencies,..

**Usage:** `ai chat track-sandbox-changes <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Absolute, or relative to the work directory; the work directory when omitted | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat read-sandbox-file`

One file of the chat's sandbox, for the viewer

**Usage:** `ai chat read-sandbox-file <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Absolute, or relative to the sandbox work directory [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-sandbox-files`

List one directory of the chat's sandbox: folders first, then files

**Usage:** `ai chat list-sandbox-files <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Absolute, or relative to the sandbox work directory | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat connect-sandbox`

Let the chat's sandbox use a project's database

**Usage:** `ai chat connect-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | The project whose services the sandbox may use [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat disconnect-sandbox`

Stop the chat's sandbox using a project's database

**Usage:** `ai chat disconnect-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat connect-sandbox-service`

Let the chat's sandbox use one service, whether or not it belongs to a project

**Usage:** `ai chat connect-sandbox-service <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | A project the service is linked to | - | No |
| `--service-id <n>` | The service the sandbox may use [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat disconnect-sandbox-service`

Stop the chat's sandbox using one service

**Usage:** `ai chat disconnect-sandbox-service <public-id> <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat wake-sandbox`

Start the chat's sandbox if it went to sleep, so the preview and the assistant's next command find it up

**Usage:** `ai chat wake-sandbox <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat list-secret-requests`

List the credentials this chat has asked for and not yet used up or lost

**Usage:** `ai chat list-secret-requests <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat provide-secret`

Provide the credential the assistant asked for

**Usage:** `ai chat provide-secret <public-id> <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--value-stdin` | The credential [required] (read from stdin) | - | No |
| `--value-file <path>` | The credential [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat discard-secret`

Discard a credential request and anything already typed into it

**Usage:** `ai chat discard-secret <public-id> <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat stop-user-turn`

Stop user turn

**Usage:** `ai chat stop-user-turn <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat get-suggestion-mode`

A chat's next-step suggestion setting, with the default it falls back to

**Usage:** `ai chat get-suggestion-mode <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat update-suggestion-mode`

Set a chat's own next-step suggestion mode, or clear it to follow the user's default

**Usage:** `ai chat update-suggestion-mode <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | What happens after each answer (one of: off, ask, always) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat suggest-next-steps`

Suggest a few next steps after the chat's latest answer

**Usage:** `ai chat suggest-next-steps <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--answer <value>` | Cursor of the answer the user is looking at, from the message list | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-conversation-usage`

What this conversation has cost so far

**Usage:** `ai chat get-user-conversation-usage <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat leave-workspace`

Take this chat out of its workspace

**Usage:** `ai chat leave-workspace <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-workspace`

The workspace this chat is in, with its other chats

**Usage:** `ai chat get-workspace <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat set-workspace`

Put this chat in a workspace, creating it when asked

**Usage:** `ai chat set-workspace <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--application-id <value>` | Body field application_id | - | No |
| `--create <json>` | A new workspace to create for the chat (JSON) | - | No |
| `--keep-sandbox <value>` | Which sandbox the chats keep when both the chat and the workspace have one (one of: workspace, chat) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-user-pending-action`

Get user pending action

**Usage:** `ai chat get-user-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat confirm-user-pending-action`

Confirm user pending action

**Usage:** `ai chat confirm-user-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat reject-user-pending-action`

Reject user pending action

**Usage:** `ai chat reject-user-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat get-suggestion-default`

The user's default for next-step suggestions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai chat update-suggestion-default`

Set the user's default for every chat that does not set its own mode

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | What happens after each answer (one of: off, ask, always) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat find-conversation`

Find the current user's existing chat for a context (returns 'null' if none yet)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--context-type <value>` | Query parameter context_type [required] | - | No |
| `--context-id <value>` | Query parameter context_id [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat create-conversation`

Get-or-create the current user's private chat for a context

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Body field ai_model | - | No |
| `--ai-permission-mode <value>` | Body field ai_permission_mode | - | No |
| `--ai-provider <value>` | Provider pinned to this conversation | - | No |
| `--ai-thinking-level <value>` | Body field ai_thinking_level | - | No |
| `--context-id <value>` | The entity id (ints stringified) [required] | - | No |
| `--context-type <value>` | e.g. '"deployment"' [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-conversations`

List the current user's active conversations for a project, most-recently-active first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-conversation`

One bounded conversation-history page (excluding internal context rows)

**Usage:** `ai chat get-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--before <value>` | Opaque next_before cursor returned by the previous page | - | No |
| `--limit <n>` | Messages per page (default 50, maximum 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat rename-conversation`

Rename a conversation (set its human-facing title)

**Usage:** `ai chat rename-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--title <value>` | New human-facing title [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat archive-conversation`

Archive (soft-delete) a conversation

**Usage:** `ai chat archive-conversation <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat send-message`

Submit a user message and start a server-owned turn

**Usage:** `ai chat send-message <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Optional next-turn model, one advertised by the conversation's provider (or by 'ai_provider' when that is set) | - | No |
| `--ai-permission-mode <value>` | Optional next-turn permission mode for the pinned provider harness | - | No |
| `--ai-provider <value>` | Optional AI gateway provider ('gateway_key:<id>') for this and later turns | - | No |
| `--ai-thinking-level <value>` | Optional next-turn thinking level | - | No |
| `--attachments <json>` | Opaque references returned by the conversation attachment endpoint (JSON) | - | No |
| `--content <value>` | Body field content [required] | - | No |
| `--page-context <value>` | Optional, client-supplied description of the page/entity the user is currently viewing (e.g. a trace in a... | - | No |
| `--turn-id <value>` | Client-generated opaque idempotency key for this turn | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat list-pending-actions`

List all pending actions for a conversation (most-recently-proposed first)

**Usage:** `ai chat list-pending-actions <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat update-permission-mode`

Update permission mode

**Usage:** `ai chat update-permission-mode <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--permission-mode <value>` | Provider permission mode to persist [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat resolve-permission`

Resolve permission

**Usage:** `ai chat resolve-permission <public-id> <permission-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--decision <json>` | The user's decision for a pending permission request (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat stop-turn`

Explicitly cancel the server-owned active turn

**Usage:** `ai chat stop-turn <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat get-pending-action`

Get a single pending action by its public id (scoped to the project)

**Usage:** `ai chat get-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat confirm-pending-action`

Confirm a proposed AI action: validate permission, atomically claim, execute, persist outcome

**Usage:** `ai chat confirm-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai chat reject-pending-action`

Reject a proposed AI action (no execution)

**Usage:** `ai chat reject-pending-action <action-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `ai provider`

Manage the AI CLIs (Claude Code, Codex) Temps runs on this server

**Subcommands:**

- `login` - Sign an AI CLI in to your subscription. Prints a link to open, then asks for the code (Claude) or shows a device code (Codex)

#### `ai provider login`

Sign an AI CLI in to your subscription. Prints a link to open, then asks for the code (Claude) or shows a device code (Codex)

**Usage:** `ai provider login <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Print the final sign-in state as JSON | - | No |

### `ai readiness`

Show which AI prerequisites this project meets, and how to fix the rest

**Usage:** `ai readiness [project] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

### `ai apps`

Apps commands

**Subcommands:**

- `list-applications` - List applications
- `create-application` - Create application
- `archive-application` - Archive an AI application
- `get-application` - Get application
- `rename-application` - Rename an AI application
- `list-application-conversations` - List application conversations
- `create-application-conversation` - Create application conversation
- `list-thread-artifacts` - List thread artifacts
- `create-thread-artifact` - Create thread artifact
- `list-application-git-connections` - List application git connections
- `bind-application-git-connection` - Bind application git connection
- `disconnect-application-git-connection` - Disconnect application git connection
- `create-application-preview-link` - Mint a short-lived URL for a port running in the application harness sandbox
- `create-application-project` - Create and link an application project
- `link-application` - Link an existing project to an application
- `unlink-application` - Unlink an application project
- `deploy-application-workspace` - Deploy an application workspace project with Drop
- `set-application-primary` - Choose the application's primary project
- `write-application-workspace-files` - Write a bounded batch of local files into an application project
- `import-application-workspace-git` - Import a Git repository into an application project
- `restore-application` - Restore an archived AI application
- `get-application-workspace` - Get application workspace
- `update-application-workspace` - Update desired application workspace resources
- `control-application-workspace` - Control an application workspace
- `get-application-workspace-changes` - Get application workspace changes
- `get-application-workspace-diff` - Get application workspace diff
- `get-application-workspace-directory` - Get application workspace directory
- `get-application-workspace-file` - Get application workspace file
- `download-application-workspace-file` - Download one application workspace file
- `upload-application-workspace-files` - Upload files into an application workspace
- `attach-application-workspace-terminal` - Attach an interactive terminal to an application workspace
- `blueprint` - Blueprint commands

#### `ai apps list-applications`

List applications

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Resource lifecycle state (defaults to active) (one of: active, archived) | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of applications or conversations per page (clamped to 1..=100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps create-application`

Create application

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--project-ids <n>` | Body field project_ids (repeatable) | - | No |
| `--starter-project <json>` | Body field starter_project (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps archive-application`

Archive an AI application

**Usage:** `ai apps archive-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application`

Get application

**Usage:** `ai apps get-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Resource lifecycle state (defaults to active) (one of: active, archived) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps rename-application`

Rename an AI application

**Usage:** `ai apps rename-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <value>` | New name shown for the application [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps list-application-conversations`

List application conversations

**Usage:** `ai apps list-application-conversations <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Resource lifecycle state (defaults to active) (one of: active, archived) | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of applications or conversations per page (clamped to 1..=100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps create-application-conversation`

Create application conversation

**Usage:** `ai apps create-application-conversation <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ai-provider <value>` | A registered development harness (for example 'claude_cli') | - | No |
| `--kind <value>` | 'ai' (default) or 'terminal' | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps list-thread-artifacts`

List thread artifacts

**Usage:** `ai apps list-thread-artifacts <application-public-id> <conversation-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai apps create-thread-artifact`

Create thread artifact

**Usage:** `ai apps create-thread-artifact <application-public-id> <conversation-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--kind <value>` | Body field kind [required] | - | No |
| `--payload <json>` | Body field payload (JSON) [required] | - | No |
| `--title <value>` | Body field title | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps list-application-git-connections`

List application git connections

**Usage:** `ai apps list-application-git-connections <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai apps bind-application-git-connection`

Bind application git connection

**Usage:** `ai apps bind-application-git-connection <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--connection-id <n>` | Body field connectionId [required] | - | No |
| `--project-id <n>` | Body field projectId [required] | - | No |
| `--remote-name <value>` | Body field remoteName [required] | - | No |
| `--repository-id <n>` | Body field repositoryId [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps disconnect-application-git-connection`

Disconnect application git connection

**Usage:** `ai apps disconnect-application-git-connection <application-public-id> <binding-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps create-application-preview-link`

Mint a short-lived URL for a port running in the application harness sandbox

**Usage:** `ai apps create-application-preview-link <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Optional same-origin path to open after the preview grant is exchanged | - | No |
| `--port <n>` | The development server port detected or selected in the sandbox [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps create-application-project`

Create and link an application project

**Usage:** `ai apps create-application-project <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--exposed-port <n>` | Body field exposed_port | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--preset <value>` | Deployment preset for the project | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps link-application`

Link an existing project to an application

**Usage:** `ai apps link-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Body field project_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps unlink-application`

Unlink an application project

**Usage:** `ai apps unlink-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps deploy-application-workspace`

Deploy an application workspace project with Drop

**Usage:** `ai apps deploy-application-workspace <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Target environment | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps set-application-primary`

Choose the application's primary project

**Usage:** `ai apps set-application-primary <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps write-application-workspace-files`

Write a bounded batch of local files into an application project

**Usage:** `ai apps write-application-workspace-files <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--files <json>` | Body field files (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps import-application-workspace-git`

Import a Git repository into an application project

**Usage:** `ai apps import-application-workspace-git <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--depth <n>` | Body field depth | - | No |
| `--git-connection-id <n>` | Opaque user-owned connection reference | - | No |
| `--revision <value>` | Body field revision | - | No |
| `--url <value>` | Body field url [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps restore-application`

Restore an archived AI application

**Usage:** `ai apps restore-application <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace`

Get application workspace

**Usage:** `ai apps get-application-workspace <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai apps update-application-workspace`

Update desired application workspace resources

**Usage:** `ai apps update-application-workspace <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cpu-limit <number>` | Body field cpu_limit | - | No |
| `--disk-limit-mb <n>` | Body field disk_limit_mb | - | No |
| `--idle-timeout-secs <n>` | Body field idle_timeout_secs | - | No |
| `--image <value>` | Pinned Temps-managed daemon image | - | No |
| `--memory-limit-mb <n>` | Body field memory_limit_mb | - | No |
| `--pids-limit <n>` | Body field pids_limit | - | No |
| `--runtime <value>` | Body field runtime | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps control-application-workspace`

Control an application workspace

**Usage:** `ai apps control-application-workspace <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--action <value>` | restart, pause, resume, rebuild, snapshot, restore, or update_runtime [required] | - | No |
| `--confirm` | Must be true for update_runtime because compute is replaced | - | No |
| `--no-confirm` | Set confirm to false | - | No |
| `--label <value>` | Body field label | - | No |
| `--runtime <value>` | Optional built-in flavor to select during update_runtime | - | No |
| `--snapshot-id <value>` | Body field snapshot_id | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace-changes`

Get application workspace changes

**Usage:** `ai apps get-application-workspace-changes <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cursor <n>` | Position returned by the previous page | - | No |
| `--limit <n>` | Files per page (1-200, default 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace-diff`

Get application workspace diff

**Usage:** `ai apps get-application-workspace-diff <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace-directory`

Get application workspace directory

**Usage:** `ai apps get-application-workspace-directory <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Workspace-relative directory; omit for the root | - | No |
| `--cursor <n>` | Position returned by the previous page | - | No |
| `--limit <n>` | Entries per page (1-100, default 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps get-application-workspace-file`

Get application workspace file

**Usage:** `ai apps get-application-workspace-file <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps download-application-workspace-file`

Download one application workspace file

**Usage:** `ai apps download-application-workspace-file <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps upload-application-workspace-files`

Upload files into an application workspace

**Usage:** `ai apps upload-application-workspace-files <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--files <json>` | Body field files (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps attach-application-workspace-terminal`

Attach an interactive terminal to an application workspace

**Usage:** `ai apps attach-application-workspace-terminal <application-public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--thread <value>` | Public id of the terminal thread to attach to [required] | - | No |
| `--cols <n>` | Initial terminal width in columns (default 80) | - | No |
| `--rows <n>` | Initial terminal height in rows (default 24) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai apps blueprint`

Blueprint commands

**Subcommands:**

- `get` - The workspace's architecture blueprint with the live state of its parts
- `unbind-component` - Remove a part's binding
- `bind-component` - Bind a part of the accepted blueprint to the project or service that realises it
- `list-versions` - Every version of the workspace's blueprint, newest first
- `save-version` - Save a version the user wrote
- `accept-version` - Accept a proposed version: every chat of the workspace builds against it
- `reject-version` - Reject a proposed version

##### `ai apps blueprint get`

The workspace's architecture blueprint with the live state of its parts

**Usage:** `ai apps blueprint get <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint unbind-component`

Remove a part's binding

**Usage:** `ai apps blueprint unbind-component <public-id> <component-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint bind-component`

Bind a part of the accepted blueprint to the project or service that realises it

**Usage:** `ai apps blueprint bind-component <public-id> <component-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Body field project_id | - | No |
| `--service-id <n>` | Body field service_id | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint list-versions`

Every version of the workspace's blueprint, newest first

**Usage:** `ai apps blueprint list-versions <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | 1-based page | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint save-version`

Save a version the user wrote

**Usage:** `ai apps blueprint save-version <public-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--change-summary <value>` | Body field change_summary [required] | - | No |
| `--document <json>` | The design of an application: what it is for, its parts and its flows (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint accept-version`

Accept a proposed version: every chat of the workspace builds against it

**Usage:** `ai apps blueprint accept-version <public-id> <version> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai apps blueprint reject-version`

Reject a proposed version

**Usage:** `ai apps blueprint reject-version <public-id> <version> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `ai coding-agents`

Coding agents commands

**Subcommands:**

- `list` - Every coding-agent CLI Temps manages, its latest published release, and whether newer releases are..
- `update-settings` - Turn automatic coding-agent updates on or off

#### `ai coding-agents list`

Every coding-agent CLI Temps manages, its latest published release, and whether newer releases are..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--refresh` | Ask the npm registry again instead of using the six-hour cache (at most once a minute) | - | No |
| `--no-refresh` | Set refresh to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai coding-agents update-settings`

Turn automatic coding-agent updates on or off

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--auto-update` | Body field auto_update | - | No |
| `--no-auto-update` | Set auto_update to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `ai gateway`

Gateway commands

**Subcommands:**

- `limits` - Limits commands
- `catalog` - Catalog commands
- `pricing` - Pricing commands
- `providers` - Providers commands
- `usage` - Usage commands
- `create-batch` - Create batch
- `retrieve-batch` - Retrieve batch
- `cancel-batch` - Cancel batch
- `chat-completions` - Chat completions
- `embeddings` - Embeddings
- `upload-file` - Upload file
- `delete-file` - Delete file
- `retrieve-file` - Retrieve file
- `file-content` - File content
- `list-models` - List models
- `create-response-json` - Explicit JSON contract for generated clients; the standard OpenAI route remains polymorphic
- `create-response-stream` - Explicit SSE contract for generated clients; never buffers a successful event stream
- `stream-structured-output` - Stream structured output

#### `ai gateway limits`

Limits commands

**Subcommands:**

- `get-overview` - Get limits overview
- `update-instance` - Update instance limits
- `reset` - Reset project limits
- `get` - Get project limits
- `update` - Update project limits

##### `ai gateway limits get-overview`

Get limits overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page of project overrides (default 1) | - | No |
| `--page-size <n>` | Page size (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway limits update-instance`

Update instance limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--max-cost-per-month-microcents <n>` | Operator-funded spend per UTC calendar month, in microcents (1/10000 of a cent; 1 USD = 1,000,000 microcents) | - | No |
| `--max-requests-per-minute <n>` | Requests per rolling minute | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway limits reset`

Reset project limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway limits get`

Get project limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway limits update`

Update project limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--max-cost-per-month-microcents <n>` | Operator-funded spend per UTC calendar month, in microcents (1/10000 of a cent; 1 USD = 1,000,000 microcents) | - | No |
| `--max-requests-per-minute <n>` | Requests per rolling minute | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway catalog`

Catalog commands

**Subcommands:**

- `get-model` - The runtime model catalog's settings and the outcome of its last refresh
- `list-model-entries` - The stored catalog's models, newest release first
- `refresh-model` - Fetch the catalog now, store it and apply its prices
- `update-model-settings` - Change where the catalog comes from and how it is refreshed
- `list-unpriced-models` - Models enabled on a provider key that no price covers

##### `ai gateway catalog get-model`

The runtime model catalog's settings and the outcome of its last refresh

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway catalog list-model-entries`

The stored catalog's models, newest release first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--provider <value>` | Temps provider id ('openai', 'anthropic', 'gemini', 'xai', 'openrouter') | - | No |
| `--search <value>` | Case-insensitive part of the model id or name | - | No |
| `--page <n>` | Page number, from 1 (default 1) | - | No |
| `--page-size <n>` | Page size (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway catalog refresh-model`

Fetch the catalog now, store it and apply its prices

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway catalog update-model-settings`

Change where the catalog comes from and how it is refreshed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--auto-refresh` | Body field auto_refresh | - | No |
| `--no-auto-refresh` | Set auto_refresh to false | - | No |
| `--discover-provider-models` | Body field discover_provider_models | - | No |
| `--no-discover-provider-models` | Set discover_provider_models to false | - | No |
| `--refresh-interval-hours <n>` | Body field refresh_interval_hours | - | No |
| `--source-url <value>` | A custom 'https://' catalog URL, or an empty string to go back to the catalog published in the Temps... | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway catalog list-unpriced-models`

Models enabled on a provider key that no price covers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway pricing`

Pricing commands

**Subcommands:**

- `get` - Get pricing

##### `ai gateway pricing get`

Get pricing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway providers`

Providers commands

**Subcommands:**

- `list-keys` - List provider keys
- `create-key` - Create provider key
- `discover-models` - Discover provider models
- `test-key-inline` - Test provider key inline
- `delete-key` - Delete provider key
- `get-key` - Get provider key
- `update-key` - Update provider key
- `add-model` - Add provider model
- `refresh-models` - Refresh provider models
- `delete-model` - Delete provider model
- `update-model` - Update provider model
- `test-key-by-id` - Test provider key by id

##### `ai gateway providers list-keys`

List provider keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers create-key`

Create provider key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-key-stdin` | Provider API key (read from stdin) | - | No |
| `--api-key-file <path>` | Provider API key (read from a file) | - | No |
| `--base-url <value>` | Required for "openai_compatible", e.g. "http://10.0.0.5:8000/v1" | - | No |
| `--capabilities <json>` | Capabilities an administrator declared for a self-hosted key (JSON) | - | No |
| `--default-model <value>` | Model id to pin for this provider (e.g. "gpt-6.2-sol") | - | No |
| `--display-name <value>` | Body field display_name [required] | - | No |
| `--provider <value>` | "openai", "anthropic", "xai", "gemini", "openrouter", or "openai_compatible" for a self-hosted... [required] | - | No |
| `--trust-private-address` | Trust the exact private 'ip:port' of 'base_url' for this key only, without adding it to the instance-wide... | - | No |
| `--no-trust-private-address` | Set trust_private_address to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers discover-models`

Discover provider models

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-key-stdin` | API key, if the server requires one (read from stdin) | - | No |
| `--api-key-file <path>` | API key, if the server requires one (read from a file) | - | No |
| `--base-url <value>` | Base URL of the server (required for "openai_compatible") | - | No |
| `--provider <value>` | Provider ID, e.g. "openai_compatible" [required] | - | No |
| `--trust-private-address` | Discover as if the base URL's exact private address were trusted on the key | - | No |
| `--no-trust-private-address` | Set trust_private_address to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers test-key-inline`

Test provider key inline

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-key-stdin` | The raw API key to test (may be empty for a self-hosted provider) (read from stdin) | - | No |
| `--api-key-file <path>` | The raw API key to test (may be empty for a self-hosted provider) (read from a file) | - | No |
| `--base-url <value>` | Optional custom base URL (required for "openai_compatible") | - | No |
| `--default-model <value>` | Model to send the test completion to | - | No |
| `--provider <value>` | Provider ID: "openai", "anthropic", "xai", "gemini", "openrouter", "openai_compatible" [required] | - | No |
| `--trust-private-address` | Test as if the base URL's exact private address were trusted on the key | - | No |
| `--no-trust-private-address` | Set trust_private_address to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers delete-key`

Delete provider key

**Usage:** `ai gateway providers delete-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers get-key`

Get provider key

**Usage:** `ai gateway providers get-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers update-key`

Update provider key

**Usage:** `ai gateway providers update-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-key-stdin` | A credential (read from stdin) | - | No |
| `--api-key-file <path>` | A credential (read from a file) | - | No |
| `--base-url <value>` | Double-Option: absent = leave unchanged, present-null = clear (revert to the provider's default endpoint),... | - | No |
| `--capabilities <json>` | Capabilities an administrator declared for a self-hosted key (JSON) | - | No |
| `--default-model <value>` | Double-Option: absent = leave unchanged, present-null = clear the pinned model (revert to the per-provider... | - | No |
| `--display-name <value>` | Body field display_name | - | No |
| `--is-active` | Body field is_active | - | No |
| `--no-is-active` | Set is_active to false | - | No |
| `--trust-private-address` | 'true' trusts the exact private 'ip:port' of the (new) base URL for this key only (requires... | - | No |
| `--no-trust-private-address` | Set trust_private_address to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers add-model`

Add provider model

**Usage:** `ai gateway providers add-model <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--display-name <value>` | Body field display_name | - | No |
| `--model-id <value>` | Body field model_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers refresh-models`

Refresh provider models

**Usage:** `ai gateway providers refresh-models <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers delete-model`

Delete provider model

**Usage:** `ai gateway providers delete-model <id> <model-row-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers update-model`

Update provider model

**Usage:** `ai gateway providers update-model <id> <model-row-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--is-enabled` | Body field is_enabled [required] | - | No |
| `--no-is-enabled` | Set is_enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway providers test-key-by-id`

Test provider key by id

**Usage:** `ai gateway providers test-key-by-id <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway usage`

Usage commands

**Subcommands:**

- `get-by` - Get usage by project
- `get-by-provider` - Get usage by provider
- `get-conversations` - Get conversations
- `get-conversation-detail` - Get conversation detail
- `get-recent` - Get usage recent
- `get-summary` - Get usage summary
- `get-timeseries` - Get usage timeseries
- `get-top-models` - Get usage top models

##### `ai gateway usage get-by`

Get usage by project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--limit <n>` | Max projects (defaults to 50, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-by-provider`

Get usage by provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-conversations`

Get conversations

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--limit <n>` | Max results (defaults to 50, max 100) | - | No |
| `--user-id <n>` | Filter by user ID | - | No |
| `--tags <value>` | Filter by tags (comma-separated) | - | No |
| `--model <value>` | Filter by model name | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-conversation-detail`

Get conversation detail

**Usage:** `ai gateway usage get-conversation-detail <conversation-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max results (defaults to 100) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-recent`

Get usage recent

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Page size (defaults to 20, max 50) | - | No |
| `--offset <n>` | Number of results to skip for pagination (defaults to 0) | - | No |
| `--provider <value>` | Filter by provider name | - | No |
| `--model <value>` | Filter by model name | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--status <n>` | Filter by HTTP status code (exact match) | - | No |
| `--cost-gte <n>` | Cost greater-than-or-equal, in microcents | - | No |
| `--cost-gt <n>` | Cost strictly greater-than, in microcents | - | No |
| `--cost-lte <n>` | Cost less-than-or-equal, in microcents | - | No |
| `--cost-lt <n>` | Cost strictly less-than, in microcents | - | No |
| `--tokens-gte <n>` | Total tokens greater-than-or-equal | - | No |
| `--tokens-gt <n>` | Total tokens strictly greater-than | - | No |
| `--tokens-lte <n>` | Total tokens less-than-or-equal | - | No |
| `--tokens-lt <n>` | Total tokens strictly less-than | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-summary`

Get usage summary

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-timeseries`

Get usage timeseries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--bucket <value>` | Bucket size: hour, day, week (defaults to day) | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--json` | Output in JSON format | - | No |

##### `ai gateway usage get-top-models`

Get usage top models

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from <value>` | ISO 8601 start time (defaults to 24h ago) | - | No |
| `--to <value>` | ISO 8601 end time (defaults to now) | - | No |
| `--limit <n>` | Max results (defaults to 10) | - | No |
| `--project-id <n>` | Filter by project ID (deployment-token usage; instance administrators only) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway create-batch`

Create batch

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--completion-window <value>` | Currently only '24h' [required] | - | No |
| `--endpoint <value>` | Endpoint every request in the input file targets: '/v1/responses', '/v1/chat/completions' or '/v1/embeddings' [required] | - | No |
| `--input-file-id <value>` | A file uploaded through 'POST /ai/v1/files' by the same caller [required] | - | No |
| `--metadata <json>` | Body field metadata (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway retrieve-batch`

Retrieve batch

**Usage:** `ai gateway retrieve-batch <batch-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway cancel-batch`

Cancel batch

**Usage:** `ai gateway cancel-batch <batch-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway chat-completions`

Chat completions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--frequency-penalty <number>` | Body field frequency_penalty | - | No |
| `--max-tokens <n>` | Body field max_tokens | - | No |
| `--messages <json>` | Body field messages (JSON) [required] | - | No |
| `--model <value>` | Body field model [required] | - | No |
| `--n <n>` | Body field n | - | No |
| `--presence-penalty <number>` | Body field presence_penalty | - | No |
| `--response-format <json>` | Body field response_format (JSON) | - | No |
| `--seed <n>` | Body field seed | - | No |
| `--stop <json>` | Body field stop (JSON) | - | No |
| `--stream` | Body field stream | - | No |
| `--no-stream` | Set stream to false | - | No |
| `--temperature <number>` | Body field temperature | - | No |
| `--tool-choice <json>` | Body field tool_choice (JSON) | - | No |
| `--tools <json>` | Body field tools (JSON) | - | No |
| `--top-p <number>` | Body field top_p | - | No |
| `--user <value>` | Body field user | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway embeddings`

Embeddings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--dimensions <n>` | Body field dimensions | - | No |
| `--encoding-format <value>` | Body field encoding_format | - | No |
| `--body-input <json>` | Body field input (JSON) [required] | - | No |
| `--model <value>` | Body field model [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway upload-file`

Upload file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--file <path>` | A JSONL batch input file: one request per line, every line targeting the same model and endpoint [required] | - | No |
| `--purpose <value>` | Must be 'batch' [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway delete-file`

Delete file

**Usage:** `ai gateway delete-file <file-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway retrieve-file`

Retrieve file

**Usage:** `ai gateway retrieve-file <file-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway file-content`

File content

**Usage:** `ai gateway file-content <file-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway list-models`

List models

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai gateway create-response-json`

Explicit JSON contract for generated clients; the standard OpenAI route remains polymorphic

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--background` | Run asynchronously and poll for the result | - | No |
| `--no-background` | Set background to false | - | No |
| `--conversation <json>` | Attach the response to a stored conversation (JSON) | - | No |
| `--model <value>` | Model to run, e.g. 'gpt-6-luna' [required] | - | No |
| `--previous-response-id <value>` | Continue from a stored response | - | No |
| `--stream` | Stream the reply as server-sent events | - | No |
| `--no-stream` | Set stream to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai gateway create-response-stream`

Explicit SSE contract for generated clients; never buffers a successful event stream

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--background` | Run asynchronously and poll for the result | - | No |
| `--no-background` | Set background to false | - | No |
| `--conversation <json>` | Attach the response to a stored conversation (JSON) | - | No |
| `--model <value>` | Model to run, e.g. 'gpt-6-luna' [required] | - | No |
| `--previous-response-id <value>` | Continue from a stored response | - | No |
| `--stream` | Stream the reply as server-sent events | - | No |
| `--no-stream` | Set stream to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Accepted for symmetry: events are always JSON lines | - | No |

#### `ai gateway stream-structured-output`

Stream structured output

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--cache-key <value>` | Optional screen-owned cache identity | - | No |
| `--cache-ttl-seconds <n>` | Backend cache lifetime in seconds (default 60, clamped to 1-300) | - | No |
| `--model <value>` | Optional enabled model id for the selected key | - | No |
| `--prompt <value>` | The complete, already-authorized context and instructions for the model [required] | - | No |
| `--provider-key-id <n>` | Optional configured gateway key | - | No |
| `--purpose <value>` | Stable product use-case identifier, for example 'api_traffic.summary' [required] | - | No |
| `--refresh` | Bypass any matching backend cache entry and replace it with this result | - | No |
| `--no-refresh` | Set refresh to false | - | No |
| `--schema <json>` | JSON Schema describing the required response object (JSON) [required] | - | No |
| `--thinking-level <value>` | Optional normalized reasoning depth advertised for the selected model | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Accepted for symmetry: events are always JSON lines | - | No |

### `ai provider-status`

Provider status commands

**Subcommands:**

- `get` - Get ai provider status
- `refresh` - Refresh ai provider status
- `update-summary-preference` - Update ai summary preference

#### `ai provider-status get`

Get ai provider status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai provider-status refresh`

Refresh ai provider status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai provider-status update-summary-preference`

Update ai summary preference

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--model <value>` | Body field model | - | No |
| `--provider-id <value>` | Body field provider_id | - | No |
| `--thinking-level <value>` | Body field thinking_level | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `ai workspace`

Workspace commands

**Subcommands:**

- `get-global` - Get global ai workspace
- `get-activity` - Aggregate all active threads for the caller's global workspace and up to 100 requested, visible..
- `get-global-changes` - Get global workspace changes
- `get-global-diff` - Get global workspace diff
- `get-global-directory` - Get global workspace directory
- `get-global-file` - Get global workspace file
- `get-file-limits` - Get effective AI workspace file limits
- `download-global-file` - Download one global AI workspace file
- `upload-global-files` - Upload files into the global AI workspace
- `create-global-preview-link` - Mint a short-lived preview URL for a port in the user's shared global AI workspace
- `attach-global-terminal` - Attach an interactive terminal to the user's global workspace

#### `ai workspace get-global`

Get global ai workspace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-activity`

Aggregate all active threads for the caller's global workspace and up to 100 requested, visible..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--application-public-ids <value>` | Comma-separated application public IDs (maximum 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-global-changes`

Get global workspace changes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cursor <n>` | Position returned by the previous page | - | No |
| `--limit <n>` | Files per page (1-200, default 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-global-diff`

Get global workspace diff

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-global-directory`

Get global workspace directory

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Workspace-relative directory; omit for the root | - | No |
| `--cursor <n>` | Position returned by the previous page | - | No |
| `--limit <n>` | Entries per page (1-100, default 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-global-file`

Get global workspace file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace get-file-limits`

Get effective AI workspace file limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ai workspace download-global-file`

Download one global AI workspace file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Query parameter path [required] | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace upload-global-files`

Upload files into the global AI workspace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--files <json>` | Body field files (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace create-global-preview-link`

Mint a short-lived preview URL for a port in the user's shared global AI workspace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Optional same-origin path to open after the preview grant is exchanged | - | No |
| `--port <n>` | The development server port detected or selected in the sandbox [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ai workspace attach-global-terminal`

Attach an interactive terminal to the user's global workspace

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--thread <value>` | Public id of the terminal thread to attach to [required] | - | No |
| `--cols <n>` | Initial terminal width in columns (default 80) | - | No |
| `--rows <n>` | Initial terminal height in rows (default 24) | - | No |
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

**Usage:** `instances remove <name>`

### `instances switch` (alias: `use`)

Switch to a different instance

**Usage:** `instances switch <name>`

### `instances show`

Show instance details (or current instance)

**Usage:** `instances show [name] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `env:pull`

Pull environment variables to a .env file

**Usage:** `env:pull [file] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Pull from specific environment | - | No |
| `-p, --project <project>` | Project slug | - | No |

## `env:push`

Push environment variables from a .env file

**Usage:** `env:push [file] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <names>` | Comma-separated environment names | - | No |
| `-p, --project <project>` | Project slug | - | No |
| `--overwrite` | Overwrite existing variables | - | No |

## `rollback`

Rollback to a previous deployment

**Usage:** `rollback [project] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-e, --environment <env>` | Target environment | `production` | No |
| `--to <id>` | Rollback to specific deployment ID | - | No |
| `-y, --yes` | Skip confirmation | - | No |

## `open`

Open project URL in browser

**Usage:** `open [project] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-e, --environment <env>` | Open specific environment | - | No |
| `--dashboard` | Open the dashboard instead of the project URL | - | No |

## `cloud`

Temps Cloud

**Subcommands:**

- `login` - Login to Temps Cloud
- `logout` - Logout from Temps Cloud
- `whoami` - Show current Temps Cloud account
- `status` - Show this self-hosted instance's Temps Cloud link
- `connect` - Connect this self-hosted instance using an enrollment code
- `disconnect` - Disconnect this self-hosted instance from Temps Cloud
- `backup-schedule` - The backup schedule that writes to the Temps Cloud destination
- `vps` - Manage cloud VPS instances
- `billing` - Manage Temps Cloud billing and subscription
- `console-access` - Console access through Temps Cloud (ADR-045) -- Cloud members with the owner/admin role can open this console with no inbound port
- `telemetry` - Where a project’s spans are written — this instance, or Temps Cloud (ADR-041)
- `get-ai-capability` - Get cloud ai capability
- `reconcile-backup-source` - Reconcile cloud backup source
- `features` - Turn the Temps Cloud features this instance uses on or off; omitted ones keep their setting
- `get-capability` - Get cloud capability

### `cloud login`

Login to Temps Cloud

### `cloud logout`

Logout from Temps Cloud

### `cloud whoami`

Show current Temps Cloud account

### `cloud status`

Show this self-hosted instance's Temps Cloud link

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output JSON | - | No |

### `cloud connect`

Connect this self-hosted instance using an enrollment code

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--code <code>` | Single-use enrollment code from Temps Cloud | - | Yes |

### `cloud disconnect`

Disconnect this self-hosted instance from Temps Cloud

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `cloud backup-schedule`

The backup schedule that writes to the Temps Cloud destination

**Subcommands:**

- `ensure` - Create the nightly default schedule unless one already targets Temps Cloud

#### `cloud backup-schedule ensure`

Create the nightly default schedule unless one already targets Temps Cloud

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output JSON | - | No |

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

**Usage:** `cloud vps show <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps destroy`

Destroy a VPS instance

**Usage:** `cloud vps destroy <id>`

#### `cloud vps retry`

Retry failed VPS provisioning

**Usage:** `cloud vps retry <id>`

#### `cloud vps credentials`

Show VPS panel credentials

**Usage:** `cloud vps credentials <id> [options]`

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

### `cloud console-access`

Console access through Temps Cloud (ADR-045) -- Cloud members with the owner/admin role can open this console with no inbound port

**Subcommands:**

- `status` - Show whether Temps Cloud can open this console
- `enable` - Allow Temps Cloud to open this console
- `disable` - Stop Temps Cloud from opening this console (revokes its sign-in provider and sessions immediately)

#### `cloud console-access status`

Show whether Temps Cloud can open this console

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output JSON | - | No |

#### `cloud console-access enable`

Allow Temps Cloud to open this console

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output JSON | - | No |

#### `cloud console-access disable`

Stop Temps Cloud from opening this console (revokes its sign-in provider and sessions immediately)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output JSON | - | No |

### `cloud telemetry`

Where a project’s spans are written — this instance, or Temps Cloud (ADR-041)

**Subcommands:**

- `write-mode` - Read or change a project’s telemetry write mode
- `status` - Instance-wide Cloud telemetry write status: queue depth, gaps, and whether the local span store is still required
- `bulk-switch` - Switch many projects to Temps Cloud and ship their history in one job — estimates first, then asks
- `bulk-status` - Show the Temps Cloud activation running on this instance — progress, ETA, skips and failures
- `bulk-cancel` - Stop a Temps Cloud activation at its next chunk boundary

#### `cloud telemetry write-mode`

Read or change a project’s telemetry write mode

**Subcommands:**

- `get` - Show where a project’s spans are written, what is queued, and any gaps
- `set` - Set the write mode to "local" (stored on this instance) or "cloud" (written to Temps Cloud, not stored here)

##### `cloud telemetry write-mode get`

Show where a project’s spans are written, what is queued, and any gaps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

##### `cloud telemetry write-mode set`

Set the write mode to "local" (stored on this instance) or "cloud" (written to Temps Cloud, not stored here)

**Usage:** `cloud telemetry write-mode set <mode> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug | - | No |
| `--fidelity <tier>` | Also set Cloud telemetry fidelity: metered or queryable. "cloud" requires "queryable". | - | No |
| `--analytics <mode>` | Also set where analytics and metrics are written: local or cloud | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |
| `--json` | Output in JSON format | - | No |

#### `cloud telemetry status`

Instance-wide Cloud telemetry write status: queue depth, gaps, and whether the local span store is still required

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cloud telemetry bulk-switch`

Switch many projects to Temps Cloud and ship their history in one job — estimates first, then asks

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--all` | Every project still storing its spans on this instance. Projects already on Temps Cloud are not included. | - | No |
| `-p, --project <id>` | A project id to switch. Repeatable. Cannot be combined with --all. | `` | No |
| `--from <timestamp>` | Start of the history window to ship (RFC 3339). Defaults to the oldest span local retention can still be holding. | - | No |
| `--to <timestamp>` | End of the history window to ship (RFC 3339). Defaults to now. | - | No |
| `-y, --yes` | Skip the confirmation. The estimate is still computed and printed. | - | No |
| `--watch` | Follow the job until it finishes | - | No |
| `--json` | Output in JSON format | - | No |

#### `cloud telemetry bulk-status`

Show the Temps Cloud activation running on this instance — progress, ETA, skips and failures

**Usage:** `cloud telemetry bulk-status [batch_id] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--watch` | Follow the job until it finishes | - | No |
| `--json` | Output in JSON format | - | No |

#### `cloud telemetry bulk-cancel`

Stop a Temps Cloud activation at its next chunk boundary

**Usage:** `cloud telemetry bulk-cancel <batch_id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `cloud get-ai-capability`

Get cloud ai capability

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `cloud reconcile-backup-source`

Reconcile cloud backup source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `cloud features`

Turn the Temps Cloud features this instance uses on or off; omitted ones keep their setting

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--backups-enabled` | Off-site backups through Temps Cloud | - | No |
| `--no-backups-enabled` | Set backups_enabled to false | - | No |
| `--console-access-enabled` | Let Temps Cloud open this console (see `temps cloud console-access`) | - | No |
| `--no-console-access-enabled` | Set console_access_enabled to false | - | No |
| `--notifications-enabled` | Notifications delivered through Temps Cloud | - | No |
| `--no-notifications-enabled` | Set notifications_enabled to false | - | No |
| `--telemetry-enabled` | Cloud telemetry for spans, analytics and metrics | - | No |
| `--no-telemetry-enabled` | Set telemetry_enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `cloud get-capability`

Get cloud capability

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `plugin`

Create and build TypeScript plugins, or install and manage external plugins

**Subcommands:**

- `install` - Install a GitHub TypeScript plugin on the configured Temps server; the server uses its host Git credentials and Docker
- `update` - Rebuild an installed GitHub plugin from its stored source; keep the current plugin if the update fails
- `install-local` - Upload and activate a trusted native plugin on the configured Temps host
- `grants` - Inspect or replace a plugin's host API permissions and AI limits
- `dev` - Run a plugin with a local simulated host, UI preview, and events (no Temps server)
- `init` - Create a TypeScript plugin project
- `build` - Build every platform selected in package.json; --all selects all six supported targets
- `publish` - Build, publish native npm packages, verify ownership and submit for review; resumes interrupted releases
- `api` - Api commands

### `plugin install`

Install a GitHub TypeScript plugin on the configured Temps server; the server uses its host Git credentials and Docker

**Usage:** `plugin install <repository> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Plugin subdirectory containing package.json and bun.lock (default: repository root) | - | No |
| `--name <name>` | Advanced: require this plugin name (otherwise auto-detected) | - | No |
| `--ref <ref>` | Advanced: branch, tag, or commit (otherwise repository default branch) | - | No |
| `-y, --yes` | Trust the repository and allow installation without prompting | - | No |
| `--grant <permissions...>` | Explicitly approve declared host permissions (default: none); e.g. ai_generate projects_read | - | No |
| `--ai-daily-calls <count>` | Maximum AI attempts per day (0 pauses usage; default: 100) | - | No |
| `--ai-max-tokens <count>` | Maximum output tokens per AI call (1–4096; default: 1024) | - | No |

### `plugin update`

Rebuild an installed GitHub plugin from its stored source; keep the current plugin if the update fails

**Usage:** `plugin update <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ref <ref>` | Use a different branch, tag, or commit | - | No |
| `-y, --yes` | Trust the update without prompting | - | No |

### `plugin install-local`

Upload and activate a trusted native plugin on the configured Temps host

**Usage:** `plugin install-local <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Expected plugin manifest name | - | Yes |
| `--plugin-version <version>` | Expected plugin manifest version | - | Yes |
| `-y, --yes` | Approve execution of this file without prompting | - | No |

### `plugin grants`

Inspect or replace a plugin's host API permissions and AI limits

**Subcommands:**

- `get` - Show current grants, actor identity, and AI availability
- `set` - Replace all host grants; --clear revokes all permissions immediately

#### `plugin grants get`

Show current grants, actor identity, and AI availability

**Usage:** `plugin grants get <name>`

#### `plugin grants set`

Replace all host grants; --clear revokes all permissions immediately

**Usage:** `plugin grants set <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--grant <permissions...>` | Complete list of permissions to grant | - | No |
| `--ai-daily-calls <count>` | Daily AI call limit (0–10000) | - | No |
| `--ai-max-tokens <count>` | Maximum AI output tokens per call (1–4096) | - | No |
| `--clear` | Revoke all host permissions | - | No |

### `plugin dev`

Run a plugin with a local simulated host, UI preview, and events (no Temps server)

**Usage:** `plugin dev [binary] [args...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Local session name | `default` | No |
| `--exec <command>` | Run a command with arguments after -- | - | No |
| `--port <port>` | Loopback port (0 selects a free port) | `0` | No |
| `--data-dir <path>` | Persistent plugin data directory | - | No |
| `--grant <permission...>` | Explicit host grants; defaults to none or saved session grants | - | No |
| `--fixtures <file>` | Version-1 JSON host fixtures and mock AI settings | - | No |
| `--role <role>` | Synthetic preview role: admin or reader | `admin` | No |
| `--startup-timeout <ms>` | Handshake deadline | `10000` | No |

**Subcommands:**

- `events` - List host event fixtures and example payloads
- `status` - Show local plugin state
- `logs` - Show the last 200 redacted simulator records
- `emit` - Deliver an event; sent does not mean handler completed
- `grants` - Change simulated host permissions immediately

#### `plugin dev events`

List host event fixtures and example payloads

#### `plugin dev status`

Show local plugin state

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Running session | - | No |
| `--json` | Machine-readable output | - | No |

#### `plugin dev logs`

Show the last 200 redacted simulator records

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Running session | - | No |
| `--json` | Machine-readable output | - | No |

#### `plugin dev emit`

Deliver an event; sent does not mean handler completed

**Usage:** `plugin dev emit [type] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Running session | - | No |
| `--file <path>` | Full JSON event envelope with a stable ID | - | No |
| `--project-id <id>` | Project ID | `1` | No |
| `--environment-id <id>` | Environment ID | `1` | No |
| `--deployment-id <id>` | Deployment ID | `42` | No |
| `--environment <name>` | Environment | `production` | No |
| `--url <url>` | Deployment URL | - | No |
| `--repeat <n>` | Repeat the same event ID, up to 1000 | - | No |
| `--count <n>` | Deliver distinct IDs, up to 1000 | - | No |
| `--transport <transport>` | auto or http | `auto` | No |
| `--json` | Machine-readable receipts and envelope | - | No |

#### `plugin dev grants`

Change simulated host permissions immediately

**Subcommands:**

- `set` - Replace all grants or revoke all with --clear

##### `plugin dev grants set`

Replace all grants or revoke all with --clear

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session <name>` | Running session | - | No |
| `--grant <permission...>` | Complete replacement grant set | - | No |
| `--clear` | Revoke every grant | - | No |

### `plugin init`

Create a TypeScript plugin project

**Usage:** `plugin init <directory> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Scoped npm name, e.g. @your-scope/my-plugin | - | Yes |

### `plugin build`

Build every platform selected in package.json; --all selects all six supported targets

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--all` | Build all supported targets | - | No |

### `plugin publish`

Build, publish native npm packages, verify ownership and submit for review; resumes interrupted releases

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Confirm public npm publication non-interactively | - | No |

### `plugin api`

Api commands

**Subcommands:**

- `list-chat-watches` - List chat plugin watches
- `change-chat-watch` - Change chat plugin watch
- `get-ai-tools` - Get plugin ai tools
- `put-ai-tools` - Put plugin ai tools

#### `plugin api list-chat-watches`

List chat plugin watches

**Usage:** `plugin api list-chat-watches <chat> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `plugin api change-chat-watch`

Change chat plugin watch

**Usage:** `plugin api change-chat-watch <chat> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--action <value>` | Body field action (one of: create, pause, resume, remove) [required] | - | No |
| `--digest <value>` | Body field digest | - | No |
| `--mode <value>` | Body field mode (one of: notify, wake) | - | No |
| `--plugin <value>` | Body field plugin | - | No |
| `--source <value>` | Body field source | - | No |
| `--subject <value>` | Body field subject | - | No |
| `--where <json>` | Body field where (JSON) | - | No |
| `--id <value>` | Body field id | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `plugin api get-ai-tools`

Get plugin ai tools

**Usage:** `plugin api get-ai-tools <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `plugin api put-ai-tools`

Put plugin ai tools

**Usage:** `plugin api put-ai-tools <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--actor-id <value>` | Current actor_id returned by GET; replacing the installation requires fresh consent [required] | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--installation-revision <value>` | Binary revision returned by GET; a changed binary requires fresh consent [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

## `setup`

PoC: install Temps on an existing Linux VPS over SSH and save a client context

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ssh <destination>` | SSH alias or user@hostname (trusted host key required) | - | Yes |
| `--email <email>` | Admin email and certificate contact | - | No |
| `--context <name>` | New local context name | `production` | No |
| `--port <port>` | SSH port | `22` | No |
| `--identity <path>` | SSH private key path (otherwise use your SSH agent/config) | - | No |
| `--channel <channel>` | Runtime release channel: stable, beta, nightly | `stable` | No |
| `--runtime-version <tag>` | Pin the runtime release | - | No |
| `--dry-run` | Show the plan without connecting, changing files or sending telemetry | - | No |
| `--telemetry` | Opt in to coarse setup-step analytics for this attempt only | - | No |
| `--no-telemetry` | Do not send setup analytics (default) | - | No |
| `-y, --yes` | Approve the installation plan without prompting | - | No |

## `start`

Run Temps on this computer, sign in the CLI and add the Temps skill for your coding agent

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--channel <channel>` | Release channel for a fresh install: stable, beta or nightly | `beta` | No |
| `--runtime-version <tag>` | Run this exact Temps release | - | No |
| `--allow-unsigned` | Run a release that publishes no Sigstore signature (only releases from before signing), trusting its checksum alone | - | No |
| `--skills <scope>` | Where to add the Temps agent skill: project, global or none (default: project inside a project, otherwise global) | - | No |
| `--port <port>` | Port for your apps and the console on a fresh install (default: 8080 or the next free port) | - | No |
| `--make-default` | Make "local" the default CLI context even if another one is active | - | No |
| `--json` | Print the result as JSON, for scripts and agents | - | No |

## `stop`

Stop the Temps server and database started by `temps start`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--delete-data` | Also delete everything in your local Temps: its apps, services, sandboxes, database and admin password (asks first) | - | No |
| `-y, --yes` | Do not ask before deleting data | - | No |

## `agents`

Project agents, their MCP servers, skills and AI providers

**Subcommands:**

- `create` - Create agent
- `get-cli-status` - Get cli status
- `list-all-runs` - List all runs
- `latest-run-for-source` - Latest run for source
- `cancel-run` - Cancel run
- `retry-run` - Retry a completed, failed, cancelled, or no_fix run with the same trigger context
- `get-sandbox-status` - Get sandbox status
- `smoke-test` - Run a smoke test to verify the selected AI CLI works in the environment where agents will actually execute..
- `delete` - Delete agent
- `get` - Get agent
- `update` - Update agent
- `list-runs` - List agent runs
- `stream-events` - SSE endpoint: streams run log events in real-time
- `list-mcps` - List mcps
- `create-mcp` - Create mcp
- `delete-mcp` - Delete mcp
- `get-mcp` - Get mcp
- `update-mcp` - Update mcp
- `reveal-mcp-config` - Reveal mcp config
- `get-skill` - Get skill
- `download-skill-archive` - Download a skill's archive (tar.gz) — project-scoped
- `save-token` - Save an encrypted AI provider token for use in sandbox containers
- `list-ai-providers` - List the AI provider catalog
- `update-ai-provider` - Update provider-scoped settings without touching the saved credential
- `activate-ai-provider` - Activate a provider as the platform-wide default
- `save-ai-provider-credential` - Save (or replace) a provider's credential
- `import-local-ai-provider-credential` - Import the credential already used by this provider's CLI on the Temps host
- `verify-saved-ai-provider-credential` - Verify a previously imported OpenCode credential against a user-selected model without asking the browser..
- `get-ai-provider-harness-update` - Installed and latest version of the provider's harness CLI in your persistent workspace, and whether an..
- `update-ai-provider-harness` - Install the latest release of the provider's harness CLI in your persistent workspace so newly released..
- `refresh-ai-provider-models` - Refresh exactly one provider's account-aware model inventory
- `run-ai-provider-preflight` - Run ai provider preflight
- `run-ai-provider-smoke` - Run ai provider smoke
- `list-global-mcps` - List global mcps
- `create-global-mcp` - Create global mcp
- `delete-global-mcp` - Delete global mcp
- `get-global-mcp` - Get global mcp
- `update-global-mcp` - Update global mcp
- `reveal-global-mcp-config` - Reveal global mcp config
- `rebuild-sandbox-image` - Rebuild sandbox image
- `get-global-sandbox-status` - Get global sandbox status
- `get-global-skill` - Get global skill
- `download-global-skill-archive` - Download a skill's archive (tar.gz) — global

### `agents create`

Create agent

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Preferred model identifier for the CLI. 'Some("")' clears the stored value | - | No |
| `--ai-provider <value>` | Body field ai_provider | - | No |
| `--ai-provider-key-id <n>` | Body field ai_provider_key_id | - | No |
| `--api-key-stdin` | Plain-text API key — will be encrypted before storage (read from stdin) | - | No |
| `--api-key-file <path>` | Plain-text API key — will be encrypted before storage (read from a file) | - | No |
| `--branch-prefix <value>` | Body field branch_prefix | - | No |
| `--config-repo-branch <value>` | Branch of the config repo to use (default: "main") | - | No |
| `--config-repo-url <value>` | Private config repo containing .claude/ directory (skills, MCP, plugins) | - | No |
| `--cooldown-minutes <n>` | Body field cooldown_minutes | - | No |
| `--daily-budget-cents <n>` | Body field daily_budget_cents | - | No |
| `--deliverable <value>` | Body field deliverable | - | No |
| `--description <value>` | Body field description | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--max-turns <n>` | Body field max_turns | - | No |
| `--mcp-servers-config <json>` | MCP servers config (Claude Code settings.json mcpServers format) (JSON) | - | No |
| `--name <value>` | Body field name | - | No |
| `--prompt <value>` | Body field prompt | - | No |
| `--sandbox-enabled` | Body field sandbox_enabled | - | No |
| `--no-sandbox-enabled` | Set sandbox_enabled to false | - | No |
| `--skills-config <json>` | Skills config as JSON array (JSON) | - | No |
| `--slug <value>` | Body field slug | - | No |
| `--timeout-seconds <n>` | Body field timeout_seconds | - | No |
| `--tools-config <json>` | Tools config as JSON array (JSON) | - | No |
| `--trigger-config <json>` | Trigger configuration JSON: { "error": { "new_issue": true, "regression": true }, "manual": true } (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-cli-status`

Get cli status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--provider <value>` | AI provider: claude_cli or codex_cli | - | No |
| `--json` | Output in JSON format | - | No |

### `agents list-all-runs`

List all runs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (1-based) | - | No |
| `--page-size <n>` | Page size (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents latest-run-for-source`

Latest run for source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--trigger-source-type <value>` | Trigger source type, e.g. 'error_group' [required] | - | No |
| `--trigger-source-id <n>` | Trigger source ID [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `agents cancel-run`

Cancel run

**Usage:** `agents cancel-run <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `agents retry-run`

Retry a completed, failed, cancelled, or no_fix run with the same trigger context

**Usage:** `agents retry-run <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-sandbox-status`

Get sandbox status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents smoke-test`

Run a smoke test to verify the selected AI CLI works in the environment where agents will actually execute..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--provider-id <value>` | Provider id to test; defaults to the globally active provider | - | No |
| `--json` | Output in JSON format | - | No |

### `agents delete`

Delete agent

**Usage:** `agents delete <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get`

Get agent

**Usage:** `agents get <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents update`

Update agent

**Usage:** `agents update <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ai-model <value>` | Preferred model identifier for the CLI. 'Some("")' clears the stored value | - | No |
| `--ai-provider <value>` | Body field ai_provider | - | No |
| `--ai-provider-key-id <n>` | Body field ai_provider_key_id | - | No |
| `--api-key-stdin` | Plain-text API key — will be encrypted before storage (read from stdin) | - | No |
| `--api-key-file <path>` | Plain-text API key — will be encrypted before storage (read from a file) | - | No |
| `--branch-prefix <value>` | Body field branch_prefix | - | No |
| `--config-repo-branch <value>` | Branch of the config repo to use (default: "main") | - | No |
| `--config-repo-url <value>` | Private config repo containing .claude/ directory (skills, MCP, plugins) | - | No |
| `--cooldown-minutes <n>` | Body field cooldown_minutes | - | No |
| `--daily-budget-cents <n>` | Body field daily_budget_cents | - | No |
| `--deliverable <value>` | Body field deliverable | - | No |
| `--description <value>` | Body field description | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--max-turns <n>` | Body field max_turns | - | No |
| `--mcp-servers-config <json>` | MCP servers config (Claude Code settings.json mcpServers format) (JSON) | - | No |
| `--name <value>` | Body field name | - | No |
| `--prompt <value>` | Body field prompt | - | No |
| `--sandbox-enabled` | Body field sandbox_enabled | - | No |
| `--no-sandbox-enabled` | Set sandbox_enabled to false | - | No |
| `--skills-config <json>` | Skills config as JSON array (JSON) | - | No |
| `--slug <value>` | Body field slug | - | No |
| `--timeout-seconds <n>` | Body field timeout_seconds | - | No |
| `--tools-config <json>` | Tools config as JSON array (JSON) | - | No |
| `--trigger-config <json>` | Trigger configuration JSON: { "error": { "new_issue": true, "regression": true }, "manual": true } (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents list-runs`

List agent runs

**Usage:** `agents list-runs <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (1-based) | - | No |
| `--page-size <n>` | Page size (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents stream-events`

SSE endpoint: streams run log events in real-time

**Usage:** `agents stream-events <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Accepted for symmetry: events are always JSON lines | - | No |

### `agents list-mcps`

List mcps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents create-mcp`

Create mcp

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--slug <value>` | Body field slug [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents delete-mcp`

Delete mcp

**Usage:** `agents delete-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-mcp`

Get mcp

**Usage:** `agents get-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents update-mcp`

Update mcp

**Usage:** `agents update-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents reveal-mcp-config`

Reveal mcp config

**Usage:** `agents reveal-mcp-config <slug> <field> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-skill`

Get skill

**Usage:** `agents get-skill <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents download-skill-archive`

Download a skill's archive (tar.gz) — project-scoped

**Usage:** `agents download-skill-archive <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

### `agents save-token`

Save an encrypted AI provider token for use in sandbox containers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--token-stdin` | The OAuth token from 'claude setup-token' or an API key [required] (read from stdin) | - | No |
| `--token-file <path>` | The OAuth token from 'claude setup-token' or an API key [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents list-ai-providers`

List the AI provider catalog

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--catalog-only` | Return only static/cached catalog metadata | - | No |
| `--no-catalog-only` | Set catalog_only to false | - | No |
| `--json` | Output in JSON format | - | No |

### `agents update-ai-provider`

Update provider-scoped settings without touching the saved credential

**Usage:** `agents update-ai-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--default-model <value>` | New default model id. 'None' or an empty string clears the stored value so the CLI falls back to its own... | - | No |
| `--max-turns-analysis <n>` | Default max turns for the autofixer analysis phase (1–200). '0' clears the stored value (built-in default... | - | No |
| `--max-turns-feedback <n>` | Default max turns for autofixer feedback rounds (1–200). '0' clears; omitted leaves unchanged | - | No |
| `--max-turns-fix <n>` | Default max turns for the autofixer fix phase (1–200). '0' clears; omitted leaves unchanged | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents activate-ai-provider`

Activate a provider as the platform-wide default

**Usage:** `agents activate-ai-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents save-ai-provider-credential`

Save (or replace) a provider's credential

**Usage:** `agents save-ai-provider-credential <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--auth-type <value>` | Auth flavor id (must match one of the provider's catalog entries) [required] | - | No |
| `--credential-stdin` | Plaintext credential body (API key, OAuth token, or full config file contents) [required] (read from stdin) | - | No |
| `--credential-file <path>` | Plaintext credential body (API key, OAuth token, or full config file contents) [required] (read from a file) | - | No |
| `--verification-model <value>` | OpenCode model to test, in 'provider/model' form | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents import-local-ai-provider-credential`

Import the credential already used by this provider's CLI on the Temps host

**Usage:** `agents import-local-ai-provider-credential <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--verification-model <value>` | OpenCode model to use for the verification request, in 'provider/model' form | - | No |
| `--json` | Output in JSON format | - | No |

### `agents verify-saved-ai-provider-credential`

Verify a previously imported OpenCode credential against a user-selected model without asking the browser..

**Usage:** `agents verify-saved-ai-provider-credential <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--verification-model <value>` | OpenCode model in 'provider/model' form, used to verify the saved secret [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-ai-provider-harness-update`

Installed and latest version of the provider's harness CLI in your persistent workspace, and whether an..

**Usage:** `agents get-ai-provider-harness-update <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--refresh` | Look inside the workspace again instead of returning the cached check | - | No |
| `--no-refresh` | Set refresh to false | - | No |
| `--cached-only` | Return only what was last read, never looking inside the workspace (which could create or wake it) | - | No |
| `--no-cached-only` | Set cached_only to false | - | No |
| `--json` | Output in JSON format | - | No |

### `agents update-ai-provider-harness`

Install the latest release of the provider's harness CLI in your persistent workspace so newly released..

**Usage:** `agents update-ai-provider-harness <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents refresh-ai-provider-models`

Refresh exactly one provider's account-aware model inventory

**Usage:** `agents refresh-ai-provider-models <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents run-ai-provider-preflight`

Run ai provider preflight

**Usage:** `agents run-ai-provider-preflight <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents run-ai-provider-smoke`

Run ai provider smoke

**Usage:** `agents run-ai-provider-smoke <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--consent` | Explicit acknowledgement that this check makes a billable model request [required] | - | No |
| `--no-consent` | Set consent to false | - | No |
| `--model <value>` | Optional catalog model identifier | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents list-global-mcps`

List global mcps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents create-global-mcp`

Create global mcp

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--slug <value>` | Body field slug [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents delete-global-mcp`

Delete global mcp

**Usage:** `agents delete-global-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `agents get-global-mcp`

Get global mcp

**Usage:** `agents get-global-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents update-global-mcp`

Update global mcp

**Usage:** `agents update-global-mcp <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--description <value>` | Body field description | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `agents reveal-global-mcp-config`

Reveal global mcp config

**Usage:** `agents reveal-global-mcp-config <slug> <field> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents rebuild-sandbox-image`

Rebuild sandbox image

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Accepted for symmetry: events are always JSON lines | - | No |

### `agents get-global-sandbox-status`

Get global sandbox status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents get-global-skill`

Get global skill

**Usage:** `agents get-global-skill <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `agents download-global-skill-archive`

Download a skill's archive (tar.gz) — global

**Usage:** `agents download-global-skill-archive <slug> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

## `shield`

Shield: bot and abuse protection rules, entries and proposals

**Subcommands:**

- `list-events` - Recent Shield events for an environment
- `lookup-client` - Look up what Shield says about a client
- `get-summary` - Shield summary for an environment
- `list-tokens` - List bypass tokens for an environment
- `create-token` - Create a bypass token
- `revoke-token` - Revoke a bypass token
- `list-entries` - List a project's Shield entries
- `create-entry` - Add a Shield entry to a project
- `get-entry` - Show one of a project's Shield entries
- `delete-entry` - Remove a project's Shield entry
- `update-entry` - Change a project's Shield entry
- `get-settings` - Get the platform Shield settings
- `update-settings` - Change the platform Shield settings
- `list-platform-entries` - List platform Shield entries
- `create-platform-entry` - Add a platform Shield entry
- `get-platform-entry` - Show one platform-wide Shield entry
- `delete-platform-entry` - Remove a platform Shield entry
- `update-platform-entry` - Change a platform Shield entry
- `list-proposals` - List fleet learning's proposals
- `approve-proposal` - Approve a proposal
- `reject-proposal` - Reject a proposal

### `shield list-events`

Recent Shield events for an environment

**Usage:** `shield list-events <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--limit <n>` | Events to return, 1 to 100; 20 by default | - | No |
| `--kind <value>` | Only this kind: 'verdict', 'challenge_issued', 'challenge_solved' or 'challenge_failed' | - | No |
| `--verdict <value>` | Only this verdict: 'block', 'challenge' or 'log' | - | No |
| `--ip <value>` | Only this client address, IPv4 or IPv6 | - | No |
| `--before <value>` | Only events before this instant (the 'next_before' of the previous page) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield lookup-client`

Look up what Shield says about a client

**Usage:** `shield lookup-client <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ip <value>` | A client address | - | No |
| `--ja4 <value>` | A JA4 TLS fingerprint | - | No |
| `--json` | Output in JSON format | - | No |

### `shield get-summary`

Shield summary for an environment

**Usage:** `shield get-summary <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--window <value>` | '1h', '24h' (the default) or '7d' (one of: 1h, 24h, 7d) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield list-tokens`

List bypass tokens for an environment

**Usage:** `shield list-tokens <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield create-token`

Create a bypass token

**Usage:** `shield create-token <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--name <value>` | What uses it: 'uptime monitor', 'CI' [required] | - | No |
| `--project-wide` | Valid for every environment of the project, not only this one | - | No |
| `--no-project-wide` | Set project_wide to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield revoke-token`

Revoke a bypass token

**Usage:** `shield revoke-token <environment-id> <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `shield list-entries`

List a project's Shield entries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield create-entry`

Add a Shield entry to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expires-at <value>` | When the entry stops applying; omit for never | - | No |
| `--ja4 <value>` | A JA4 fingerprint; exactly one of 'network' and 'ja4' | - | No |
| `--network <value>` | An address or CIDR range; exactly one of 'network' and 'ja4' | - | No |
| `--note <value>` | Body field note | - | No |
| `--verdict <value>` | The verdict an operator entry gives what it matches (one of: allow, log, challenge, block) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield get-entry`

Show one of a project's Shield entries

**Usage:** `shield get-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield delete-entry`

Remove a project's Shield entry

**Usage:** `shield delete-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `shield update-entry`

Change a project's Shield entry

**Usage:** `shield update-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expires-at <value>` | Body field expires_at | - | No |
| `--note <value>` | Body field note | - | No |
| `--verdict <value>` | The verdict an operator entry gives what it matches (one of: allow, log, challenge, block) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield get-settings`

Get the platform Shield settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `shield update-settings`

Change the platform Shield settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--share-observations` | Whether what one environment's visitors did counts for every environment (D9): addresses and fingerprints... | - | No |
| `--no-share-observations` | Set share_observations to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield list-platform-entries`

List platform Shield entries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `shield create-platform-entry`

Add a platform Shield entry

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expires-at <value>` | When the entry stops applying; omit for never | - | No |
| `--ja4 <value>` | A JA4 fingerprint; exactly one of 'network' and 'ja4' | - | No |
| `--network <value>` | An address or CIDR range; exactly one of 'network' and 'ja4' | - | No |
| `--note <value>` | Body field note | - | No |
| `--verdict <value>` | The verdict an operator entry gives what it matches (one of: allow, log, challenge, block) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield get-platform-entry`

Show one platform-wide Shield entry

**Usage:** `shield get-platform-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `shield delete-platform-entry`

Remove a platform Shield entry

**Usage:** `shield delete-platform-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `shield update-platform-entry`

Change a platform Shield entry

**Usage:** `shield update-platform-entry <entry-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expires-at <value>` | Body field expires_at | - | No |
| `--note <value>` | Body field note | - | No |
| `--verdict <value>` | The verdict an operator entry gives what it matches (one of: allow, log, challenge, block) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `shield list-proposals`

List fleet learning's proposals

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status <value>` | Only proposals in this state; all of them when omitted (one of: pending, approved, rejected) | - | No |
| `--q <value>` | Only proposals whose address or fingerprint contains this text, ignoring case | - | No |
| `--page <n>` | Page number, from 1 (the default) | - | No |
| `--page-size <n>` | Proposals per page: 25 by default, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

### `shield approve-proposal`

Approve a proposal

**Usage:** `shield approve-proposal <proposal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `shield reject-proposal`

Reject a proposal

**Usage:** `shield reject-proposal <proposal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `vault`

Access vaults, the services they hold and their sessions

**Subcommands:**

- `list-services` - List access services general
- `create-service` - Create access service general
- `delete-service` - Delete access service general
- `get-service` - Get access service general
- `update-service` - Update access service general
- `sessions` - Sessions commands
- `list` - List access vaults general
- `create` - Create access vault general
- `delete` - Delete access vault general
- `get` - Get access vault general
- `update` - Update access vault general
- `list-bundles` - List access bundles general
- `create-bundle` - Create access bundle general
- `delete-bundle` - Delete access bundle general
- `update-bundle` - Update access bundle general
- `get-eligibility` - Get vault eligibility
- `list-links` - List vault links
- `create-link` - Create vault link
- `delete-link` - Delete vault link
- `project` - Project commands
- `sandbox` - Sandbox access vaults
- `capability` - General access capability
- `service-catalog` - Service catalog

### `vault list-services`

List access services general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `vault create-service`

Create access service general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--source-project-id <n>` | Query parameter source_project_id | - | No |
| `--clear-credential` | Body field clear_credential | - | No |
| `--no-clear-credential` | Set clear_credential to false | - | No |
| `--credential-stdin` | A credential (read from stdin) | - | No |
| `--credential-file <path>` | A credential (read from a file) | - | No |
| `--enabled` | Allows requests to be saved; does not enable a network connection [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--expected-version <n>` | Required for updates; omit for creation | - | No |
| `--max-ttl-seconds <n>` | Maximum lifetime of a request, between 60 and 86400 seconds [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--policy <json>` | Exact destination and request scope (JSON) [required] | - | No |
| `--vault-id <value>` | Add a newly created service to this vault in the same transaction | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault delete-service`

Delete access service general

**Usage:** `vault delete-service <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `vault get-service`

Get access service general

**Usage:** `vault get-service <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `vault update-service`

Update access service general

**Usage:** `vault update-service <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--clear-credential` | Body field clear_credential | - | No |
| `--no-clear-credential` | Set clear_credential to false | - | No |
| `--credential-stdin` | A credential (read from stdin) | - | No |
| `--credential-file <path>` | A credential (read from a file) | - | No |
| `--enabled` | Allows requests to be saved; does not enable a network connection [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--expected-version <n>` | Required for updates; omit for creation | - | No |
| `--max-ttl-seconds <n>` | Maximum lifetime of a request, between 60 and 86400 seconds [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--policy <json>` | Exact destination and request scope (JSON) [required] | - | No |
| `--vault-id <value>` | Add a newly created service to this vault in the same transaction | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault sessions`

Sessions commands

**Subcommands:**

- `list` - List access sessions general
- `get` - Get access session general
- `revoke` - Revoke access session general
- `create` - Create access session general

#### `vault sessions list`

List access sessions general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--vault-id <value>` | Query parameter vault_id | - | No |
| `--sandbox-id <value>` | Filter by the sandbox's public ID; ownership still applies | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault sessions get`

Get access session general

**Usage:** `vault sessions get <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `vault sessions revoke`

Revoke access session general

**Usage:** `vault sessions revoke <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault sessions create`

Create access session general

**Usage:** `vault sessions create <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--bundle-id <value>` | Body field bundle_id [required] | - | No |
| `--bundle-version <n>` | Body field bundle_version [required] | - | No |
| `--consumer <json>` | Body field consumer (JSON) [required] | - | No |
| `--request-id <value>` | Stable for retries of this approval [required] | - | No |
| `--services <json>` | Exact policies the operator reviewed, one per bundle service (maximum 16) (JSON) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault list`

List access vaults general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `vault create`

Create access vault general

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | Existing services in this project (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault delete`

Delete access vault general

**Usage:** `vault delete <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `vault get`

Get access vault general

**Usage:** `vault get <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--sandbox-id <value>` | Query parameter sandbox_id | - | No |
| `--json` | Output in JSON format | - | No |

### `vault update`

Update access vault general

**Usage:** `vault update <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | Existing services in this project (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault list-bundles`

List access bundles general

**Usage:** `vault list-bundles <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--sandbox-id <value>` | Query parameter sandbox_id | - | No |
| `--json` | Output in JSON format | - | No |

### `vault create-bundle`

Create access bundle general

**Usage:** `vault create-bundle <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | A selection of this vault's services, not an authorization grant (repeatable) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault delete-bundle`

Delete access bundle general

**Usage:** `vault delete-bundle <vault-id> <bundle-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `vault update-bundle`

Update access bundle general

**Usage:** `vault update-bundle <vault-id> <bundle-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | A selection of this vault's services, not an authorization grant (repeatable) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault get-eligibility`

Get vault eligibility

**Usage:** `vault get-eligibility <vault-id> <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `vault list-links`

List vault links

**Usage:** `vault list-links <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--project-ids <value>` | Query parameter project_ids | - | No |
| `--sandbox-ids <value>` | Query parameter sandbox_ids | - | No |
| `--json` | Output in JSON format | - | No |

### `vault create-link`

Create vault link

**Usage:** `vault create-link <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Body field project_id | - | No |
| `--sandbox-id <value>` | Body field sandbox_id | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `vault delete-link`

Delete vault link

**Usage:** `vault delete-link <vault-id> <link-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `vault project`

Project commands

**Subcommands:**

- `sessions` - Sessions commands
- `list` - List access vaults
- `create` - Create access vault
- `get` - Get access vault
- `update` - Update access vault
- `list-bundles` - List access bundles
- `create-bundle` - Create access bundle
- `update-bundle` - Update access bundle
- `create-session` - Create access session
- `project` - Project access vaults

#### `vault project sessions`

Sessions commands

**Subcommands:**

- `list` - List access sessions
- `get` - Get access session
- `revoke` - Revoke access session

##### `vault project sessions list`

List access sessions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--vault-id <value>` | Query parameter vault_id | - | No |
| `--sandbox-id <value>` | Filter by the sandbox's public ID; ownership still applies | - | No |
| `--json` | Output in JSON format | - | No |

##### `vault project sessions get`

Get access session

**Usage:** `vault project sessions get <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

##### `vault project sessions revoke`

Revoke access session

**Usage:** `vault project sessions revoke <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project list`

List access vaults

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project create`

Create access vault

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | Existing services in this project (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project get`

Get access vault

**Usage:** `vault project get <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project update`

Update access vault

**Usage:** `vault project update <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | Existing services in this project (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project list-bundles`

List access bundles

**Usage:** `vault project list-bundles <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project create-bundle`

Create access bundle

**Usage:** `vault project create-bundle <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | A selection of this vault's services, not an authorization grant (repeatable) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project update-bundle`

Update access bundle

**Usage:** `vault project update-bundle <vault-id> <bundle-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--service-ids <value>` | A selection of this vault's services, not an authorization grant (repeatable) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project create-session`

Create access session

**Usage:** `vault project create-session <vault-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--bundle-id <value>` | Body field bundle_id [required] | - | No |
| `--bundle-version <n>` | Body field bundle_version [required] | - | No |
| `--consumer <json>` | Body field consumer (JSON) [required] | - | No |
| `--request-id <value>` | Stable for retries of this approval [required] | - | No |
| `--services <json>` | Exact policies the operator reviewed, one per bundle service (maximum 16) (JSON) [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `vault project project`

Project access vaults

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `vault sandbox`

Sandbox access vaults

**Usage:** `vault sandbox <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `vault capability`

General access capability

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `vault service-catalog`

Service catalog

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--search <value>` | Query parameter search | - | No |
| `--id <value>` | Query parameter id | - | No |
| `--json` | Output in JSON format | - | No |


---

## Examples

### Basic Workflow

```bash
# Login to Temps
bunx @temps-sdk/cli@beta login

# Create a new project on the intended server
bunx @temps-sdk/cli@beta --target-context production projects create --name my-app

# Deploy to production
bunx @temps-sdk/cli@beta --target-context production deploy --project my-app --environment production

# View deployment logs
bunx @temps-sdk/cli@beta deployments logs --project my-app --follow

# Stream runtime container logs
bunx @temps-sdk/cli@beta runtime-logs --project my-app

# List containers
bunx @temps-sdk/cli@beta containers list --project-id 1 --environment-id 1
```

### Managing Environments

```bash
# List environments
bunx @temps-sdk/cli@beta environments list --project my-app

# Set an environment variable on the intended server (prompts for the value)
bunx @temps-sdk/cli@beta --target-context production environments vars set --project my-app DATABASE_URL --environments production

# Store a secret from a file, or pipe it from a password manager (never type it on the command line)
bunx @temps-sdk/cli@beta --target-context production environments vars set --project my-app DB_PASSWORD --secret --value-file ./db-password.txt --environments production
op read --no-newline op://vault/db/password | bunx @temps-sdk/cli@beta --target-context production environments vars set --project my-app DB_PASSWORD --secret --value-stdin --environments production

# View environment variables
bunx @temps-sdk/cli@beta environments vars list --project my-app
```

### Managing Domains

```bash
# Request a certificate for a domain on the intended server
bunx @temps-sdk/cli@beta --target-context production domains add --domain app.example.com

# Route the domain to a project environment
bunx @temps-sdk/cli@beta --target-context production custom-domains create --project-id 1 --environment-id 1 --domain app.example.com

# List domains
bunx @temps-sdk/cli@beta domains list

# Remove a domain from the intended server
bunx @temps-sdk/cli@beta --target-context production domains remove --domain app.example.com
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

Use `bunx @temps-sdk/cli@beta configure show` to view current configuration.

## Support

- Documentation: https://temps.sh/docs
- Issues: https://github.com/gotempsh/temps/issues
