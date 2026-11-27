<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `projects` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`projects secrets`](#projects-secrets)
- [`projects list`](#projects-list-alias-ls)
- [`projects create`](#projects-create-alias-new)
- [`projects cloudflare-capability`](#projects-cloudflare-capability-alias-delivery-capability)
- [`projects show`](#projects-show-alias-get)
- [`projects group`](#projects-group)
- [`projects update`](#projects-update-alias-edit)
- [`projects settings`](#projects-settings)
- [`projects git`](#projects-git)
- [`projects source`](#projects-source)
- [`projects hosting`](#projects-hosting)
- [`projects container-security`](#projects-container-security)
- [`projects identical-builds`](#projects-identical-builds)
- [`projects config`](#projects-config)
- [`projects delete`](#projects-delete-alias-rm)
- [`projects create-from-template`](#projects-create-from-template)
- [`projects get-statistics`](#projects-get-statistics)
- [`projects get-compose-security`](#projects-get-compose-security)
- [`projects update-compose-security`](#projects-update-compose-security)
- [`projects update-automatic-deploy`](#projects-update-automatic-deploy)
- [`projects get-resolved-environment-variables`](#projects-get-resolved-environment-variables)
- [`projects get-resolved-environment-variable-value`](#projects-get-resolved-environment-variable-value)
- [`projects get-environment-variable`](#projects-get-environment-variable)
- [`projects dismiss-environment-variable-secret-suggestion`](#projects-dismiss-environment-variable-secret-suggestion)
- [`projects get-environment`](#projects-get-environment)
- [`projects get-environment-domains`](#projects-get-environment-domains)
- [`projects add-environment-domain`](#projects-add-environment-domain)
- [`projects delete-environment-domain`](#projects-delete-environment-domain)
- [`projects update-environment-subdomain`](#projects-update-environment-subdomain)
- [`projects teardown-environment`](#projects-teardown-environment)
- [`projects reinstall-github-webhook`](#projects-reinstall-github-webhook)
- [`projects reinstall-gitlab-webhook`](#projects-reinstall-gitlab-webhook)
- [`projects get-ingest-setup`](#projects-get-ingest-setup)
- [`projects update-service-template-runtime`](#projects-update-service-template-runtime)
- [`projects get-service-template`](#projects-get-service-template)
- [`projects upgrade-service-template`](#projects-upgrade-service-template)

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
| `--storage-service-env-aliases <json>` | Variable aliases per linked storage service, keyed by service id: app name to injected name, e.g. {"12":{"DATABASE_URL":"POSTGRES_URL"}} (JSON) | - | No |
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
