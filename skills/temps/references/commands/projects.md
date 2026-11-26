<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `projects` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`projects secrets`](#projects-secrets)
- [`projects list`](#projects-list)
- [`projects create`](#projects-create)
- [`projects cloudflare-capability`](#projects-cloudflare-capability)
- [`projects show`](#projects-show)
- [`projects group`](#projects-group)
- [`projects update`](#projects-update)
- [`projects settings`](#projects-settings)
- [`projects git`](#projects-git)
- [`projects source`](#projects-source)
- [`projects hosting`](#projects-hosting)
- [`projects container-security`](#projects-container-security)
- [`projects identical-builds`](#projects-identical-builds)
- [`projects config`](#projects-config)
- [`projects delete`](#projects-delete)

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

### `projects secrets`

Manage project secrets — mounted into the deployed container as files at /run/secrets/<KEY>, not environment variables. Distinct from `temps secrets` (agent/MCP-sandbox-scoped).

**Subcommands:**

- `list` (`ls`) - List secrets for a project (values are never returned)
- `create` (`add`) - Create a project secret (mounted at /run/secrets/<KEY> on the next deployment). Read the value with --value-file or --value-stdin to keep it out of shell history
- `update` - Update a project secret (a redeploy is required for running containers to pick it up)
- `delete` (`rm`) - Delete a project secret

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

### `projects delete` (alias: `rm`)

Delete a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
